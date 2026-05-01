use crate::multibuffer_hint::MultibufferHint;
use client::{Client, UserStore, zed_urls};
use db::kvp::KeyValueStore;
use fs::Fs;
use gpui::{
    Action, AnyElement, App, AppContext, AsyncWindowContext, Context, Entity, EventEmitter,
    FocusHandle, Focusable, Global, IntoElement, KeyContext, Render, ScrollHandle, SharedString,
    Subscription, Task, WeakEntity, Window, actions, rgb,
};
use notifications::status_toast::{StatusToast, ToastIcon};
use schemars::JsonSchema;
use serde::Deserialize;
use settings::{SettingsStore, VsCodeSettingsSource};
use std::sync::Arc;
use terminal_view::TerminalView;
use ui::{
    Divider, KeyBinding, ParentElement as _, StatefulInteractiveElement, Vector, VectorName,
    WithScrollbar as _, prelude::*, rems_from_px,
};

use workspace::{
    AppState, NewCenterTerminal, Workspace, WorkspaceId,
    dock::DockPosition,
    item::{Item, ItemEvent},
    notifications::NotifyResultExt as _,
    open_new, register_serializable_item, with_active_or_new_workspace,
};
use zed_actions::OpenOnboarding;

mod base_keymap_picker;
mod basics_page;
pub mod multibuffer_hint;
mod theme_preview;

/// Imports settings from Visual Studio Code.
#[derive(Copy, Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = zerminal, deprecated_aliases = ["zed::ImportVsCodeSettings"])]
#[serde(deny_unknown_fields)]
pub struct ImportVsCodeSettings {
    #[serde(default)]
    pub skip_prompt: bool,
}

/// Imports settings from Cursor editor.
#[derive(Copy, Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = zerminal, deprecated_aliases = ["zed::ImportCursorSettings"])]
#[serde(deny_unknown_fields)]
pub struct ImportCursorSettings {
    #[serde(default)]
    pub skip_prompt: bool,
}

pub const FIRST_OPEN: &str = "first_open";
/// KVP flag persisted when the user finishes (or dismisses) the Quickstart
/// onboarding. Versioned so existing Zerminal installs — which all have
/// `first_open=false` from the previous eager-write behavior — still see the
/// rewritten Quickstart once.
pub const QUICKSTART_SEEN: &str = "quickstart_seen_v1";
pub const DOCS_URL: &str = "https://github.com/elleryfamilia/zerminal#readme";

/// Returns `true` when the Quickstart should be shown on app launch (i.e.,
/// the user has not yet completed or dismissed it).
pub fn should_show_quickstart(kvp: &KeyValueStore) -> bool {
    matches!(kvp.read_kvp(QUICKSTART_SEEN), Ok(None))
}

actions!(
    onboarding,
    [
        /// Finish the onboarding process.
        Finish,
        /// Sign in while in the onboarding flow.
        SignIn,
        /// Open the user account in zed.dev while in the onboarding flow.
        OpenAccount,
        /// Resets the welcome screen hints to their initial state.
        ResetHints
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _cx| {
        workspace
            .register_action(|_workspace, _: &ResetHints, _, cx| MultibufferHint::set_count(0, cx));
    })
    .detach();

    cx.on_action(|_: &OpenOnboarding, cx| {
        with_active_or_new_workspace(cx, |workspace, window, cx| {
            workspace
                .with_local_workspace(window, cx, |workspace, window, cx| {
                    let existing = workspace
                        .active_pane()
                        .read(cx)
                        .items()
                        .find_map(|item| item.downcast::<Onboarding>());

                    if let Some(existing) = existing {
                        workspace.activate_item(&existing, true, true, window, cx);
                    } else {
                        let settings_page = Onboarding::new(workspace, cx);
                        workspace.add_item_to_active_pane(
                            Box::new(settings_page),
                            None,
                            true,
                            window,
                            cx,
                        )
                    }
                })
                .detach();
        });
    });

    // Zerminal: ShowWelcome is intentionally not wired. The Welcome tab
    // (quick-start buttons, recent projects, etc.) is an editor-centric
    // landing page that does not fit a terminal-first product.

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|_workspace, action: &ImportVsCodeSettings, window, cx| {
            let fs = <dyn Fs>::global(cx);
            let action = *action;

            let workspace = cx.weak_entity();

            window
                .spawn(cx, async move |cx: &mut AsyncWindowContext| {
                    handle_import_vscode_settings(
                        workspace,
                        VsCodeSettingsSource::VsCode,
                        action.skip_prompt,
                        fs,
                        cx,
                    )
                    .await
                })
                .detach();
        });

        workspace.register_action(|_workspace, action: &ImportCursorSettings, window, cx| {
            let fs = <dyn Fs>::global(cx);
            let action = *action;

            let workspace = cx.weak_entity();

            window
                .spawn(cx, async move |cx: &mut AsyncWindowContext| {
                    handle_import_vscode_settings(
                        workspace,
                        VsCodeSettingsSource::Cursor,
                        action.skip_prompt,
                        fs,
                        cx,
                    )
                    .await
                })
                .detach();
        });
    })
    .detach();

    base_keymap_picker::init(cx);

    register_serializable_item::<Onboarding>(cx);
}

pub fn show_onboarding_view(app_state: Arc<AppState>, cx: &mut App) -> Task<anyhow::Result<()>> {
    telemetry::event!("Onboarding Page Opened");
    open_new(
        Default::default(),
        app_state,
        cx,
        |workspace, window, cx| {
            workspace.toggle_dock(DockPosition::Left, window, cx);
            let onboarding_page = Onboarding::new(workspace, cx);
            workspace.add_item_to_center(Box::new(onboarding_page.clone()), window, cx);
            window.focus(&onboarding_page.focus_handle(cx), cx);
            cx.notify();
        },
    )
}

struct Onboarding {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    user_store: Entity<UserStore>,
    scroll_handle: ScrollHandle,
    _settings_subscription: Subscription,
}

impl Onboarding {
    fn new(workspace: &Workspace, cx: &mut App) -> Entity<Self> {
        let font_family_cache = theme::FontFamilyCache::global(cx);

        cx.new(|cx| {
            cx.spawn(async move |this, cx| {
                font_family_cache.prefetch(cx).await;
                this.update(cx, |_, cx| {
                    cx.notify();
                })
            })
            .detach();

            Self {
                workspace: workspace.weak_handle(),
                focus_handle: cx.focus_handle(),
                scroll_handle: ScrollHandle::new(),
                user_store: workspace.user_store().clone(),
                _settings_subscription: cx
                    .observe_global::<SettingsStore>(move |_, cx| cx.notify()),
            }
        })
    }

    fn on_finish(_: &Finish, _: &mut Window, cx: &mut App) {
        telemetry::event!("Finish Setup");
        finalize_quickstart(cx);
    }

    fn handle_sign_in(&mut self, _: &SignIn, window: &mut Window, cx: &mut Context<Self>) {
        let client = Client::global(cx);
        let workspace = self.workspace.clone();

        window
            .spawn(cx, async move |mut cx| {
                client
                    .sign_in_with_optional_connect(true, &cx)
                    .await
                    .notify_workspace_async_err(workspace, &mut cx);
            })
            .detach();
    }

    fn handle_open_account(_: &OpenAccount, _: &mut Window, cx: &mut App) {
        cx.open_url(&zed_urls::account_url(cx))
    }

    fn render_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_8()
            .child(render_quickstart_overview())
            .child(Divider::horizontal().color(ui::DividerColor::BorderVariant))
            .child(crate::basics_page::render_basics_page(
                &self.user_store,
                cx,
            ))
            .into_any_element()
    }
}

fn render_quickstart_overview() -> impl IntoElement {
    v_flex()
        .gap_4()
        .child(Headline::new("What makes Zerminal different").size(HeadlineSize::Small))
        .child(render_overview_card(
            IconName::Terminal,
            "Terminal-first workspace",
            "The active terminal's working directory drives project detection. Your shell is the front door, not a sidebar afterthought.",
        ))
        .child(render_overview_card(
            IconName::Sparkle,
            "Bring your own CLI agents",
            "Run Claude Code, Codex, Aider, or any CLI agent in any tab. No Zerminal account, no hosted AI billing.",
        ))
        .child(render_overview_card(
            IconName::BookCopy,
            "Context pane",
            "Surfaces AGENTS.md, CLAUDE.md, and project notes for the agents working alongside you.",
        ))
        .child(render_overview_card(
            IconName::Thread,
            "Multi-agent workflow",
            "Run several terminal agents side by side and hand context off between them.",
        ))
}

fn render_overview_card(
    icon: IconName,
    title: &'static str,
    body: &'static str,
) -> impl IntoElement {
    h_flex()
        .gap_3()
        .items_start()
        .child(Icon::new(icon).color(Color::Accent))
        .child(
            v_flex()
                .gap_0p5()
                .child(Label::new(title))
                .child(
                    Label::new(body)
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                ),
        )
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .image_cache(gpui::retain_all("onboarding-page"))
            .key_context({
                let mut ctx = KeyContext::new_with_defaults();
                ctx.add("Onboarding");
                ctx.add("menu");
                ctx
            })
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .on_action(Self::on_finish)
            .on_action(cx.listener(Self::handle_sign_in))
            .on_action(Self::handle_open_account)
            .on_action(cx.listener(|_, _: &menu::SelectNext, window, cx| {
                window.focus_next(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|_, _: &menu::SelectPrevious, window, cx| {
                window.focus_prev(cx);
                cx.notify();
            }))
            .child(
                div()
                    .max_w(Rems(48.0))
                    .size_full()
                    .mx_auto()
                    .child(
                        v_flex()
                            .id("page-content")
                            .m_auto()
                            .p_12()
                            .size_full()
                            .max_w_full()
                            .min_w_0()
                            .gap_6()
                            .overflow_y_scroll()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_4()
                                    .justify_between()
                                    .child(
                                        h_flex()
                                            .gap_4()
                                            .child(
                                                // Render the brand as a rounded orange container
                                                // with the palm figure as a tintable SVG inside.
                                                // The palm follows `Color::Default` (= the active
                                                // theme's text color), so it inverts between
                                                // light and dark themes automatically — no
                                                // imperative theme observation needed.
                                                div()
                                                    .size(rems(2.5))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .rounded_lg()
                                                    .overflow_hidden()
                                                    .bg(rgb(0xFF956F))
                                                    .child(
                                                        Vector::square(
                                                            VectorName::ZerminalBrandmark,
                                                            rems(2.5),
                                                        )
                                                        .color(Color::Default),
                                                    ),
                                            )
                                            .child(
                                                v_flex()
                                                    .child(
                                                        Headline::new("Zerminal Quickstart")
                                                            .size(HeadlineSize::Small),
                                                    )
                                                    .child(
                                                        Label::new("Your terminal, with an editor attached")
                                                            .color(Color::Muted)
                                                            .size(LabelSize::Small)
                                                            .italic(),
                                                    ),
                                            ),
                                    )
                                    .child({
                                        Button::new("finish_setup", "Finish Setup")
                                            .style(ButtonStyle::Filled)
                                            .size(ButtonSize::Medium)
                                            .width(rems_from_px(200.))
                                            .key_binding(KeyBinding::for_action_in(
                                                &Finish,
                                                &self.focus_handle,
                                                cx,
                                            ))
                                            .on_click(|_, window, cx| {
                                                window.dispatch_action(Finish.boxed_clone(), cx);
                                            })
                                    }),
                            )
                            .child(Divider::horizontal().color(ui::DividerColor::BorderVariant))
                            .child(self.render_page(cx))
                            .track_scroll(&self.scroll_handle),
                    )
                    .vertical_scrollbar_for(&self.scroll_handle, window, cx),
            )
    }
}

impl EventEmitter<ItemEvent> for Onboarding {}

impl Focusable for Onboarding {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for Onboarding {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Quickstart".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Onboarding Page Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn can_split(&self) -> bool {
        true
    }

    fn suppresses_default_terminal(&self) -> bool {
        true
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<WorkspaceId>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>> {
        Task::ready(Some(cx.new(|cx| Onboarding {
            workspace: self.workspace.clone(),
            user_store: self.user_store.clone(),
            scroll_handle: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _settings_subscription: cx.observe_global::<SettingsStore>(move |_, cx| cx.notify()),
        })))
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(workspace::item::ItemEvent)) {
        f(*event)
    }
}

/// Closes the Quickstart tab if present, ensures the workspace has a center
/// terminal (deploying one when none exists), and persists `QUICKSTART_SEEN`
/// so the page does not show again on subsequent launches.
fn finalize_quickstart(cx: &mut App) {
    with_active_or_new_workspace(cx, |workspace, window, cx| {
        let onboarding_id = workspace
            .active_pane()
            .read(cx)
            .items()
            .find_map(|item| {
                let _ = item.downcast::<Onboarding>()?;
                Some(item.item_id())
            });

        if let Some(onboarding_id) = onboarding_id {
            workspace.active_pane().update(cx, |pane, cx| {
                pane.remove_item(onboarding_id, false, false, window, cx);
            });
        }

        let has_terminal = workspace
            .active_pane()
            .read(cx)
            .items_of_type::<TerminalView>()
            .next()
            .is_some();
        if !has_terminal {
            TerminalView::deploy(workspace, &NewCenterTerminal::default(), window, cx);
        }
    });

    let kvp = KeyValueStore::global(cx);
    db::write_and_log(cx, move || async move {
        kvp.write_kvp(QUICKSTART_SEEN.to_string(), "true".to_string())
            .await
    });
}

pub async fn handle_import_vscode_settings(
    workspace: WeakEntity<Workspace>,
    source: VsCodeSettingsSource,
    skip_prompt: bool,
    fs: Arc<dyn Fs>,
    cx: &mut AsyncWindowContext,
) {
    use util::truncate_and_remove_front;

    let vscode_settings =
        match settings::VsCodeSettings::load_user_settings(source, fs.clone()).await {
            Ok(vscode_settings) => vscode_settings,
            Err(err) => {
                zlog::error!("{err:?}");
                let _ = cx.prompt(
                    gpui::PromptLevel::Info,
                    &format!("Could not find or load a {source} settings file"),
                    None,
                    &["Ok"],
                );
                return;
            }
        };

    if !skip_prompt {
        let prompt = cx.prompt(
            gpui::PromptLevel::Warning,
            &format!(
                "Importing {} settings may overwrite your existing settings. \
                Will import settings from {}",
                vscode_settings.source,
                truncate_and_remove_front(&vscode_settings.path.to_string_lossy(), 128),
            ),
            None,
            &["Ok", "Cancel"],
        );
        let result = cx.spawn(async move |_| prompt.await.ok()).await;
        if result != Some(0) {
            return;
        }
    };

    let Ok(result_channel) = cx.update(|_, cx| {
        let source = vscode_settings.source;
        let path = vscode_settings.path.clone();
        let result_channel = cx
            .global::<SettingsStore>()
            .import_vscode_settings(fs, vscode_settings);
        zlog::info!("Imported {source} settings from {}", path.display());
        result_channel
    }) else {
        return;
    };

    let result = result_channel.await;
    workspace
        .update_in(cx, |workspace, _, cx| match result {
            Ok(_) => {
                let confirmation_toast = StatusToast::new(
                    format!("Your {} settings were successfully imported.", source),
                    cx,
                    |this, _| {
                        this.icon(ToastIcon::new(IconName::Check).color(Color::Success))
                            .dismiss_button(true)
                    },
                );
                SettingsImportState::update(cx, |state, _| match source {
                    VsCodeSettingsSource::VsCode => {
                        state.vscode = true;
                    }
                    VsCodeSettingsSource::Cursor => {
                        state.cursor = true;
                    }
                });
                workspace.toggle_status_toast(confirmation_toast, cx);
            }
            Err(_) => {
                let error_toast = StatusToast::new(
                    "Failed to import settings. See log for details",
                    cx,
                    |this, _| {
                        this.icon(ToastIcon::new(IconName::Close).color(Color::Error))
                            .action("Open Log", |window, cx| {
                                window.dispatch_action(workspace::OpenLog.boxed_clone(), cx)
                            })
                            .dismiss_button(true)
                    },
                );
                workspace.toggle_status_toast(error_toast, cx);
            }
        })
        .ok();
}

#[derive(Default, Copy, Clone)]
pub struct SettingsImportState {
    pub cursor: bool,
    pub vscode: bool,
}

impl Global for SettingsImportState {}

impl SettingsImportState {
    pub fn global(cx: &App) -> Self {
        cx.try_global().cloned().unwrap_or_default()
    }
    pub fn update<R>(cx: &mut App, f: impl FnOnce(&mut Self, &mut App) -> R) -> R {
        cx.update_default_global(f)
    }
}

impl workspace::SerializableItem for Onboarding {
    fn serialized_item_kind() -> &'static str {
        "OnboardingPage"
    }

    fn cleanup(
        workspace_id: workspace::WorkspaceId,
        alive_items: Vec<workspace::ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> gpui::Task<gpui::Result<()>> {
        workspace::delete_unloaded_items(
            alive_items,
            workspace_id,
            "onboarding_pages",
            &persistence::OnboardingPagesDb::global(cx),
            cx,
        )
    }

    fn deserialize(
        _project: Entity<project::Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: workspace::WorkspaceId,
        item_id: workspace::ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::Task<gpui::Result<Entity<Self>>> {
        let db = persistence::OnboardingPagesDb::global(cx);
        window.spawn(cx, async move |cx| {
            if let Some(_) = db.get_onboarding_page(item_id, workspace_id)? {
                workspace.update(cx, |workspace, cx| Onboarding::new(workspace, cx))
            } else {
                Err(anyhow::anyhow!("No onboarding page to deserialize"))
            }
        })
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: workspace::ItemId,
        _closing: bool,
        _window: &mut Window,
        cx: &mut ui::Context<Self>,
    ) -> Option<gpui::Task<gpui::Result<()>>> {
        let workspace_id = workspace.database_id()?;

        let db = persistence::OnboardingPagesDb::global(cx);
        Some(
            cx.background_spawn(
                async move { db.save_onboarding_page(item_id, workspace_id).await },
            ),
        )
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        event == &ItemEvent::UpdateTab
    }
}

mod persistence {
    use db::{
        query,
        sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
        sqlez_macros::sql,
    };
    use workspace::WorkspaceDb;

    pub struct OnboardingPagesDb(ThreadSafeConnection);

    impl Domain for OnboardingPagesDb {
        const NAME: &str = stringify!(OnboardingPagesDb);

        const MIGRATIONS: &[&str] = &[
            sql!(
                        CREATE TABLE onboarding_pages (
                            workspace_id INTEGER,
                            item_id INTEGER UNIQUE,
                            page_number INTEGER,

                            PRIMARY KEY(workspace_id, item_id),
                            FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                            ON DELETE CASCADE
                        ) STRICT;
            ),
            sql!(
                        CREATE TABLE onboarding_pages_2 (
                            workspace_id INTEGER,
                            item_id INTEGER UNIQUE,

                            PRIMARY KEY(workspace_id, item_id),
                            FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                            ON DELETE CASCADE
                        ) STRICT;
                        INSERT INTO onboarding_pages_2 SELECT workspace_id, item_id FROM onboarding_pages;
                        DROP TABLE onboarding_pages;
                        ALTER TABLE onboarding_pages_2 RENAME TO onboarding_pages;
            ),
        ];
    }

    db::static_connection!(OnboardingPagesDb, [WorkspaceDb]);

    impl OnboardingPagesDb {
        query! {
            pub async fn save_onboarding_page(
                item_id: workspace::ItemId,
                workspace_id: workspace::WorkspaceId
            ) -> Result<()> {
                INSERT OR REPLACE INTO onboarding_pages(item_id, workspace_id)
                VALUES (?, ?)
            }
        }

        query! {
            pub fn get_onboarding_page(
                item_id: workspace::ItemId,
                workspace_id: workspace::WorkspaceId
            ) -> Result<Option<workspace::ItemId>> {
                SELECT item_id
                FROM onboarding_pages
                WHERE item_id = ? AND workspace_id = ?
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{QUICKSTART_SEEN, should_show_quickstart};
    use db::kvp::KeyValueStore;

    #[gpui::test]
    async fn quickstart_shows_on_empty_kvp_and_hides_after_write() {
        let kvp = KeyValueStore::open_test_db("quickstart_seen_test").await;

        assert!(
            should_show_quickstart(&kvp),
            "fresh install should see the Quickstart"
        );

        kvp.write_kvp(QUICKSTART_SEEN.to_string(), "true".to_string())
            .await
            .expect("kvp write");

        assert!(
            !should_show_quickstart(&kvp),
            "Quickstart should not show again after Finish persists the flag"
        );
    }
}
