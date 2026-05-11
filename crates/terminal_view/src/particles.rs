// Pure simulation for the ambient particle screensaver.
//
// Direct port of phlx0/drift's "particles" scene
// (`internal/scene/particles/particles.go`). Constants and formulas are 1:1
// with the upstream so visual fidelity matches drift. The only deliberate
// extension is the `opacity` multiplier applied at color-emit time so the
// scene can sit behind terminal text without overwhelming it.
//
// Constants from drift (verbatim):
//   particleGlyphs = ['◦', '·', '○', '•', '.', '°', '∘']
//   flowField:
//     fx = sin(x*0.04 + t*0.25) * cos(y*0.06 + t*0.18) * 0.6
//     fy = cos(x*0.06 + t*0.20) * sin(y*0.04 + t*0.22) * 0.6
//   tick:
//     vx += fx*dt; vy += fy*dt + gravity*dt
//     (vx, vy) *= friction.powf(dt * 60.0)
//     clamp ‖v‖ ≤ 3.0
//     x += vx*dt; y += vy*dt
//     phase += 1.2*dt
//   trail[ix][iy] = max(trail[ix][iy], 0.9), per-frame decay 2.8*dt floored at 0
//   off-screen test: x < -2 || x > w+2 || y < -2 || y > h+2; respawn at random edge
//   spawn velocity: speed = 0.4 + rand()*2.2, angle = rand()*2π
//   trail color: lerp(theme.dim[i], theme.palette[i], brightness*0.45) for brightness >= 0.08
//   particle color: shimmer = 0.65 + 0.35*sin(phase),
//                   lerp(theme.palette[idx], theme.bright, shimmer*0.5)

use gpui::{Hsla, Rgba, rgb};
use rand::{Rng, SeedableRng, rngs::SmallRng};

pub const PARTICLE_GLYPHS: [char; 7] = ['◦', '·', '○', '•', '.', '°', '∘'];
const TRAIL_GLYPH: char = '·';

const MAX_SPEED: f32 = 3.0;
const TRAIL_PEAK: f32 = 0.9;
const TRAIL_DECAY_PER_SEC: f32 = 2.8;
const TRAIL_VISIBILITY_THRESHOLD: f32 = 0.08;
const PHASE_RATE: f32 = 1.2;

/// Drift theme: palette + dim variants of the same N hues, plus one bright accent.
#[derive(Clone, Copy)]
pub struct Theme {
    pub palette: &'static [Hsla],
    pub dim: &'static [Hsla],
    pub bright: Hsla,
}

#[derive(Clone, Copy, Debug)]
pub struct ParticlesCfg {
    pub count: usize,
    pub gravity: f32,
    pub friction: f32,
    pub opacity: f32,
}

impl Default for ParticlesCfg {
    fn default() -> Self {
        ParticlesCfg {
            count: 60,
            gravity: 0.0,
            friction: 0.98,
            opacity: 0.7,
        }
    }
}

#[derive(Clone, Copy)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    glyph: char,
    palette_idx: u8,
    phase: f32,
}

pub struct Particles {
    pub cols: usize,
    pub rows: usize,
    pub theme: Theme,
    pub cfg: ParticlesCfg,
    particles: Vec<Particle>,
    // Row-major, len = cols * rows.
    trail: Vec<f32>,
    time: f32,
    rng: SmallRng,
    dirty_rows: Vec<bool>,
}

impl Particles {
    pub fn new(cols: usize, rows: usize, theme: Theme, cfg: ParticlesCfg, seed: u64) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut rng = SmallRng::seed_from_u64(seed);
        let particles = (0..cfg.count)
            .map(|_| Self::spawn_at_random_edge(cols as f32, rows as f32, &theme, &mut rng))
            .collect();
        Particles {
            cols,
            rows,
            theme,
            cfg,
            particles,
            trail: vec![0.0; cols * rows],
            time: 0.0,
            rng,
            dirty_rows: vec![false; rows],
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.trail = vec![0.0; cols * rows];
        self.dirty_rows = vec![false; rows];
        // Resize repositions any out-of-bounds particles on the next tick via
        // the off-screen check; nothing to do here other than clear the trail.
    }

    pub fn dirty_rows(&self) -> &[bool] {
        &self.dirty_rows
    }

    pub fn tick(&mut self, dt: f32) {
        let dt = dt.max(0.0);
        self.time += dt;

        // Decay the trail buffer toward 0.
        let decay = TRAIL_DECAY_PER_SEC * dt;
        for value in self.trail.iter_mut() {
            *value = (*value - decay).max(0.0);
        }

        let width = self.cols as f32;
        let height = self.rows as f32;
        let friction_factor = self.cfg.friction.powf(dt * 60.0);

        for particle in self.particles.iter_mut() {
            let (fx, fy) = flow_field(particle.x, particle.y, self.time);

            particle.vx += fx * dt;
            particle.vy += fy * dt + self.cfg.gravity * dt;

            particle.vx *= friction_factor;
            particle.vy *= friction_factor;

            let speed_sq = particle.vx * particle.vx + particle.vy * particle.vy;
            if speed_sq > MAX_SPEED * MAX_SPEED {
                let scale = MAX_SPEED / speed_sq.sqrt();
                particle.vx *= scale;
                particle.vy *= scale;
            }

            particle.x += particle.vx * dt;
            particle.y += particle.vy * dt;
            particle.phase += PHASE_RATE * dt;

            if particle.x < -2.0
                || particle.x > width + 2.0
                || particle.y < -2.0
                || particle.y > height + 2.0
            {
                *particle =
                    Self::spawn_at_random_edge(width, height, &self.theme, &mut self.rng);
            }

            let ix = particle.x.round() as i32;
            let iy = particle.y.round() as i32;
            if ix >= 0 && (ix as usize) < self.cols && iy >= 0 && (iy as usize) < self.rows {
                let idx = (iy as usize) * self.cols + (ix as usize);
                if self.trail[idx] < TRAIL_PEAK {
                    self.trail[idx] = TRAIL_PEAK;
                }
            }
        }

        // Recompute dirty_rows: a row is dirty if it contains any visible
        // trail cell or any particle whose rounded row falls inside it.
        for row in self.dirty_rows.iter_mut() {
            *row = false;
        }
        for row in 0..self.rows {
            let row_start = row * self.cols;
            let row_end = row_start + self.cols;
            if self.trail[row_start..row_end]
                .iter()
                .any(|brightness| *brightness >= TRAIL_VISIBILITY_THRESHOLD)
            {
                self.dirty_rows[row] = true;
            }
        }
        for particle in self.particles.iter() {
            let iy = particle.y.round();
            if iy >= 0.0 && (iy as usize) < self.rows {
                self.dirty_rows[iy as usize] = true;
            }
        }
    }

    /// Returns the glyph + color to render at a cell, or None if the cell is
    /// empty. Particles overwrite trails (drift behavior).
    pub fn cell(&self, col: usize, row: usize) -> Option<(char, Hsla)> {
        if col >= self.cols || row >= self.rows {
            return None;
        }

        // Particle layer: linear scan. With ~60 particles per terminal this is
        // dominated by cache hits and stays cheaper than maintaining a parallel
        // index keyed on rounded position.
        for particle in self.particles.iter() {
            let ix = particle.x.round();
            let iy = particle.y.round();
            if ix as i32 == col as i32 && iy as i32 == row as i32 {
                let palette_len = self.theme.palette.len();
                if palette_len == 0 {
                    return None;
                }
                let palette_idx = (particle.palette_idx as usize) % palette_len;
                let shimmer = 0.65 + 0.35 * particle.phase.sin();
                let color = lerp_rgba(
                    self.theme.palette[palette_idx],
                    self.theme.bright,
                    (shimmer * 0.5).clamp(0.0, 1.0),
                );
                return Some((particle.glyph, with_opacity(color, self.cfg.opacity)));
            }
        }

        // Trail layer.
        let idx = row * self.cols + col;
        let brightness = self.trail[idx];
        if brightness < TRAIL_VISIBILITY_THRESHOLD {
            return None;
        }
        let palette_len = self.theme.palette.len();
        if palette_len == 0 {
            return None;
        }
        let palette_idx = (col + row) % palette_len;
        let color = lerp_rgba(
            self.theme.dim[palette_idx % self.theme.dim.len()],
            self.theme.palette[palette_idx],
            (brightness * 0.45).clamp(0.0, 1.0),
        );
        Some((TRAIL_GLYPH, with_opacity(color, self.cfg.opacity)))
    }

    fn spawn_at_random_edge(
        width: f32,
        height: f32,
        theme: &Theme,
        rng: &mut SmallRng,
    ) -> Particle {
        let edge = rng.random_range(0..4);
        let (x, y) = match edge {
            0 => (rng.random_range(0.0..width.max(1.0)), -1.0),
            1 => (rng.random_range(0.0..width.max(1.0)), height + 1.0),
            2 => (-1.0, rng.random_range(0.0..height.max(1.0))),
            _ => (width + 1.0, rng.random_range(0.0..height.max(1.0))),
        };
        let speed = 0.4 + rng.random::<f32>() * 2.2;
        let angle = rng.random::<f32>() * std::f32::consts::TAU;
        let glyph_idx = rng.random_range(0..PARTICLE_GLYPHS.len());
        let palette_idx = if theme.palette.is_empty() {
            0
        } else {
            rng.random_range(0..theme.palette.len()) as u8
        };
        Particle {
            x,
            y,
            vx: angle.cos() * speed,
            vy: angle.sin() * speed,
            glyph: PARTICLE_GLYPHS[glyph_idx],
            palette_idx,
            phase: rng.random::<f32>() * std::f32::consts::TAU,
        }
    }
}

fn flow_field(x: f32, y: f32, t: f32) -> (f32, f32) {
    let fx = (x * 0.04 + t * 0.25).sin() * (y * 0.06 + t * 0.18).cos() * 0.6;
    let fy = (x * 0.06 + t * 0.20).cos() * (y * 0.04 + t * 0.22).sin() * 0.6;
    (fx, fy)
}

/// Component-wise (1-t)*a + t*b in linear RGB, retaining the alpha-blend
/// semantics of `Lerp` in drift. NOT alpha-composite (for that see `Rgba::blend`).
pub fn lerp_rgba(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let a_rgb: Rgba = a.into();
    let b_rgb: Rgba = b.into();
    let mix = Rgba {
        r: a_rgb.r + (b_rgb.r - a_rgb.r) * t,
        g: a_rgb.g + (b_rgb.g - a_rgb.g) * t,
        b: a_rgb.b + (b_rgb.b - a_rgb.b) * t,
        a: a_rgb.a + (b_rgb.a - a_rgb.a) * t,
    };
    mix.into()
}

fn with_opacity(color: Hsla, opacity: f32) -> Hsla {
    Hsla {
        a: (color.a * opacity).clamp(0.0, 1.0),
        ..color
    }
}

// Theme palettes, copied from drift `internal/scene/scene.go` Themes map.
// Kept as `&'static [Hsla]` slices populated lazily via `OnceLock` so the
// runtime conversion from `u32` hex codes happens at most once per theme.

fn rgb_palette(hexes: &[u32]) -> Vec<Hsla> {
    hexes.iter().map(|h| rgb(*h).into()).collect()
}

// Return a static `Theme` for the given name (case-insensitive). Falls back to
// `cosmic`. The palette/dim arrays are `&'static`, populated lazily via
// `OnceLock`.
pub fn theme_by_name(name: &str) -> Theme {
    use std::sync::OnceLock;
    macro_rules! theme_lock {
        ($name:ident, $palette:expr, $dim:expr, $bright:expr) => {{
            static PALETTE: OnceLock<Vec<Hsla>> = OnceLock::new();
            static DIM: OnceLock<Vec<Hsla>> = OnceLock::new();
            static BRIGHT: OnceLock<Hsla> = OnceLock::new();
            Theme {
                palette: PALETTE.get_or_init(|| rgb_palette($palette)).as_slice(),
                dim: DIM.get_or_init(|| rgb_palette($dim)).as_slice(),
                bright: *BRIGHT.get_or_init(|| rgb($bright).into()),
            }
        }};
    }

    match name.to_ascii_lowercase().as_str() {
        "cosmic" => theme_lock!(
            cosmic,
            // palette
            &[0x66ccff, 0xff66cc, 0xffcc66, 0xcc66ff, 0x66ffcc, 0xff9966],
            // dim
            &[0x336688, 0x883366, 0x886633, 0x663388, 0x338866, 0x885533],
            // bright
            0xffffff
        ),
        "nord" => theme_lock!(
            nord,
            &[0x88c0d0, 0x81a1c1, 0x5e81ac, 0xb48ead, 0xa3be8c, 0xebcb8b],
            &[0x445566, 0x405060, 0x2e4055, 0x584055, 0x515f43, 0x756540],
            0xeceff4
        ),
        "dracula" => theme_lock!(
            dracula,
            &[0xff79c6, 0xbd93f9, 0x8be9fd, 0x50fa7b, 0xf1fa8c, 0xffb86c],
            &[0x803c63, 0x5e4980, 0x457480, 0x287d3d, 0x787d3c, 0x805c36],
            0xf8f8f2
        ),
        "catppuccin" => theme_lock!(
            catppuccin,
            &[0xf5c2e7, 0xcba6f7, 0x89b4fa, 0x94e2d5, 0xa6e3a1, 0xf9e2af],
            &[0x7a6173, 0x65537a, 0x445a7d, 0x4a716a, 0x537150, 0x7c7157],
            0xcdd6f4
        ),
        "gruvbox" => theme_lock!(
            gruvbox,
            &[0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c],
            &[0x7d2419, 0x5c5e13, 0x7d5e17, 0x41524c, 0x69434d, 0x47603e],
            0xfbf1c7
        ),
        "forest" => theme_lock!(
            forest,
            &[0x4a7c59, 0x6b8e23, 0x8fbc8f, 0xa0522d, 0xd2b48c, 0x556b2f],
            &[0x253e2c, 0x354711, 0x475e47, 0x502916, 0x695a46, 0x2a3517],
            0xf0fff0
        ),
        "wildberries" => theme_lock!(
            wildberries,
            &[0xc71585, 0x8b008b, 0xff1493, 0xff69b4, 0xda70d6, 0xba55d3],
            &[0x630a42, 0x450045, 0x7d0a48, 0x7d3458, 0x6c386b, 0x5b2a69],
            0xffe4e1
        ),
        "mono" => theme_lock!(
            mono,
            &[0x33ff33, 0x66ff66, 0x99ff99, 0x00cc00, 0x009900, 0x006600],
            &[0x195f19, 0x3a8c3a, 0x4f7d4f, 0x006400, 0x004c00, 0x003200],
            0xccffcc
        ),
        "rosepine" => theme_lock!(
            rosepine,
            &[0xebbcba, 0x9ccfd8, 0xc4a7e7, 0xf6c177, 0xeb6f92, 0x31748f],
            &[0x755e5d, 0x4e676b, 0x625473, 0x7b613b, 0x753848, 0x183a47],
            0xe0def4
        ),
        _ => theme_by_name("cosmic"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> Theme {
        theme_by_name("cosmic")
    }

    #[test]
    fn tick_advances_particle() {
        let cfg = ParticlesCfg {
            count: 4,
            ..ParticlesCfg::default()
        };
        let mut sim = Particles::new(80, 24, test_theme(), cfg, 1234);
        let initial_position = (sim.particles[0].x, sim.particles[0].y);

        for _ in 0..30 {
            sim.tick(1.0 / 30.0);
        }

        let final_position = (sim.particles[0].x, sim.particles[0].y);
        let dx = final_position.0 - initial_position.0;
        let dy = final_position.1 - initial_position.1;
        let distance = (dx * dx + dy * dy).sqrt();
        assert!(
            distance > 0.1,
            "particle should have moved by at least 0.1 cell over 1 simulated second; \
             initial={initial_position:?}, final={final_position:?}, distance={distance}"
        );
    }

    #[test]
    fn resize_does_not_panic_and_rebuilds_trail() {
        let cfg = ParticlesCfg {
            count: 8,
            ..ParticlesCfg::default()
        };
        let mut sim = Particles::new(80, 24, test_theme(), cfg, 1234);
        for _ in 0..5 {
            sim.tick(1.0 / 30.0);
        }
        sim.resize(40, 12);
        assert_eq!(sim.cols, 40);
        assert_eq!(sim.rows, 12);
        assert_eq!(sim.trail.len(), 40 * 12);
        assert_eq!(sim.dirty_rows.len(), 12);
        assert!(sim.trail.iter().all(|brightness| *brightness == 0.0));
        for _ in 0..5 {
            sim.tick(1.0 / 30.0);
        }
        // Indexing the corner cell must not panic.
        let _ = sim.cell(39, 11);
    }

    #[test]
    fn off_screen_particle_respawns_on_an_edge() {
        let cfg = ParticlesCfg {
            count: 1,
            ..ParticlesCfg::default()
        };
        let mut sim = Particles::new(80, 24, test_theme(), cfg, 1234);
        // Fling the only particle far off-screen.
        sim.particles[0].x = 1_000.0;
        sim.particles[0].y = 1_000.0;

        sim.tick(1.0 / 30.0);

        let particle = sim.particles[0];
        let on_screen = particle.x >= -2.0
            && particle.x <= sim.cols as f32 + 2.0
            && particle.y >= -2.0
            && particle.y <= sim.rows as f32 + 2.0;
        assert!(
            on_screen,
            "respawned particle should be near an edge; got ({}, {}) for {}x{}",
            particle.x, particle.y, sim.cols, sim.rows
        );
    }
}
