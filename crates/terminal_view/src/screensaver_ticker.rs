// Shared 30 FPS ticker driving the ambient particle screensaver. One
// `gpui::Global` singleton owns the registry and a single foreground task;
// each `TerminalView` registers a weak handle when it activates the
// screensaver and drops its `TickerHandle` when it dismisses.
//
// Re-entrancy hazard (per plan §5): the inner `Terminal` entity emits
// `Event::Wakeup` from its event loop, which routes through
// `subscribe_for_terminal_events` and calls `terminal_view.update`. The tick
// must NOT call into nested `view.update` while a terminal subscription is
// concurrently mutating the same view. To avoid that, visibility is read
// from a plain bool field on `TerminalView` (kept fresh by visibility hooks
// outside the tick path) before the inner `update` is called, and the tick
// itself only calls `Particles::tick` + `cx.notify()`.

use std::time::{Duration, Instant};

use gpui::{App, EntityId, Global, Task, WeakEntity};

use crate::TerminalView;

const MIN_FPS: u32 = 5;
const MAX_FPS: u32 = 60;
const MAX_DT_PER_TICK: f32 = 0.1;

#[derive(Default)]
pub struct ScreensaverTicker {
    registry: Vec<WeakEntity<TerminalView>>,
    task: Option<Task<()>>,
    fps: u32,
}

impl Global for ScreensaverTicker {}

/// Drop guard returned by `register`. Holds the entity id for prompt
/// `unregister` on dismissal; in addition, the next tick's dead-weak sweep
/// will remove the entry naturally once the view itself is dropped.
pub struct TickerHandle {
    view_id: EntityId,
}

impl TickerHandle {
    pub fn view_id(&self) -> EntityId {
        self.view_id
    }
}

/// Register a `TerminalView` weak handle with the global ticker. Spawns the
/// driver task on first registration. Returns a `TickerHandle` whose drop
/// triggers next-tick cleanup; for prompt removal callers should also call
/// [`unregister`] before dropping the handle.
pub fn register(view: WeakEntity<TerminalView>, fps: u32, cx: &mut App) -> TickerHandle {
    let view_id = view
        .upgrade()
        .map(|entity| entity.entity_id())
        .expect("registering a screensaver view that has already been dropped");
    let needs_task = {
        let ticker = cx.default_global::<ScreensaverTicker>();
        ticker.registry.push(view);
        ticker.fps = fps.clamp(MIN_FPS, MAX_FPS);
        ticker.task.is_none()
    };
    if needs_task {
        spawn_ticker_task(cx);
    }
    TickerHandle { view_id }
}

/// Remove a registered view by entity id. Safe to call even if the view is
/// not present.
pub fn unregister(view_id: EntityId, cx: &mut App) {
    if !cx.has_global::<ScreensaverTicker>() {
        return;
    }
    let ticker = cx.global_mut::<ScreensaverTicker>();
    ticker.registry.retain(|weak| {
        weak.upgrade()
            .map(|entity| entity.entity_id() != view_id)
            .unwrap_or(false)
    });
    if ticker.registry.is_empty() {
        ticker.task = None;
    }
}

fn spawn_ticker_task(cx: &mut App) {
    let fps = cx.global::<ScreensaverTicker>().fps.max(MIN_FPS);
    let task = cx.spawn(async move |cx| {
        let period = Duration::from_secs_f32(1.0 / fps as f32);
        let mut last_tick = Instant::now();
        loop {
            cx.background_executor().timer(period).await;
            let now = Instant::now();
            let dt = (now - last_tick).as_secs_f32().min(MAX_DT_PER_TICK);
            last_tick = now;

            let stop = cx.update(|cx| advance_all(dt, cx));
            if stop {
                break;
            }
        }
    });
    cx.global_mut::<ScreensaverTicker>().task = Some(task);
}

fn advance_all(dt: f32, cx: &mut App) -> bool {
    let snapshot: Vec<WeakEntity<TerminalView>> =
        cx.global::<ScreensaverTicker>().registry.clone();

    for weak in snapshot {
        let Some(view) = weak.upgrade() else { continue };
        // Snapshot the cached visibility flag WITHOUT entering `view.update`
        // — pure field read, no nested borrows of related entities.
        let visible = view.read(cx).is_visible_for_screensaver_cached();
        if !visible {
            continue;
        }
        // Snapshot the current grid dimensions before the inner update so the
        // particles layer follows window resizes without a bounce.
        let bounds = view.read(cx).terminal_bounds(cx);
        let cols = bounds.num_columns().max(1);
        let rows = bounds.num_lines().max(1);
        view.update(cx, |view, cx| {
            if let Some(particles) = view.screensaver_mut() {
                particles.resize(cols, rows);
                particles.tick(dt);
                cx.notify();
            }
        });
    }

    let ticker = cx.global_mut::<ScreensaverTicker>();
    ticker.registry.retain(|weak| weak.upgrade().is_some());
    if ticker.registry.is_empty() {
        ticker.task = None;
        true
    } else {
        false
    }
}
