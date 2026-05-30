use collections::HashMap;
use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, Hsla, ImageSource, RenderImage, StyledText,
    Task, img, pulsating_between,
};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use mermaid_rs_renderer::ir::NodeStyle;
use mermaid_rs_renderer::{DiagramKind, Graph, Node, NodeShape};
use ui::prelude::*;
use ui::{Icon, IconName, IconSize, LabelSize};

use crate::parser::{CodeBlockKind, MarkdownEvent, MarkdownTag};

use super::{Markdown, MarkdownStyle, ParsedMarkdown};

type MermaidDiagramCache = HashMap<MermaidCacheKey, Arc<CachedMermaidDiagram>>;

/// Cache key for a rendered diagram. Includes a `theme_key` so that switching
/// the editor theme re-renders the diagram (and switching back reuses the
/// previously rendered, correctly-colored image) rather than serving stale
/// colors.
#[derive(Clone, PartialEq, Eq, Hash)]
struct MermaidCacheKey {
    contents: ParsedMarkdownMermaidDiagramContents,
    theme_key: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedMarkdownMermaidDiagram {
    pub(crate) content_range: Range<usize>,
    pub(crate) contents: ParsedMarkdownMermaidDiagramContents,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ParsedMarkdownMermaidDiagramContents {
    pub(crate) contents: SharedString,
    pub(crate) scale: u32,
}

#[derive(Default, Clone)]
pub(crate) struct MermaidState {
    cache: MermaidDiagramCache,
    order: Vec<MermaidCacheKey>,
    /// Identity of the theme the cache was populated for; used at lookup time so
    /// the render path finds the entry rendered for the current theme.
    theme_key: u64,
}

struct CachedMermaidDiagram {
    render_image: Arc<OnceLock<anyhow::Result<Arc<RenderImage>>>>,
    fallback_image: Option<Arc<RenderImage>>,
    _task: Task<()>,
}

impl MermaidState {
    pub(crate) fn clear(&mut self) {
        self.cache.clear();
        self.order.clear();
    }

    fn get_fallback_image(
        idx: usize,
        old_order: &[MermaidCacheKey],
        new_order_len: usize,
        cache: &MermaidDiagramCache,
    ) -> Option<Arc<RenderImage>> {
        if old_order.len() != new_order_len {
            return None;
        }

        old_order.get(idx).and_then(|old_key| {
            cache.get(old_key).and_then(|old_cached| {
                old_cached
                    .render_image
                    .get()
                    .and_then(|result| result.as_ref().ok().cloned())
                    .or_else(|| old_cached.fallback_image.clone())
            })
        })
    }

    pub(crate) fn update(&mut self, parsed: &ParsedMarkdown, cx: &mut Context<Markdown>) {
        let theme = mermaid_theme(cx);
        let category_styles = category_styles(cx);
        let theme_key = theme_fingerprint(&theme, &category_styles);
        self.theme_key = theme_key;

        let mut new_order = Vec::new();
        for mermaid_diagram in parsed.mermaid_diagrams.values() {
            new_order.push(MermaidCacheKey {
                contents: mermaid_diagram.contents.clone(),
                theme_key,
            });
        }

        for (idx, key) in new_order.iter().enumerate() {
            if !self.cache.contains_key(key) {
                let fallback =
                    Self::get_fallback_image(idx, &self.order, new_order.len(), &self.cache);
                self.cache.insert(
                    key.clone(),
                    Arc::new(CachedMermaidDiagram::new(
                        key.contents.clone(),
                        theme.clone(),
                        category_styles.clone(),
                        fallback,
                        cx,
                    )),
                );
            }
        }

        let new_order_set: std::collections::HashSet<_> = new_order.iter().cloned().collect();
        self.cache.retain(|key, _| new_order_set.contains(key));
        self.order = new_order;
    }
}

impl CachedMermaidDiagram {
    fn new(
        contents: ParsedMarkdownMermaidDiagramContents,
        theme: mermaid_rs_renderer::Theme,
        category_styles: Vec<(&'static str, NodeStyle)>,
        fallback_image: Option<Arc<RenderImage>>,
        cx: &mut Context<Markdown>,
    ) -> Self {
        let render_image = Arc::new(OnceLock::<anyhow::Result<Arc<RenderImage>>>::new());
        let render_image_clone = render_image.clone();
        let svg_renderer = cx.svg_renderer();

        let task = cx.spawn(async move |this, cx| {
            let value = cx
                .background_spawn(async move {
                    // The renderer silently produces a garbled diagram for v11
                    // node-metadata syntax rather than erroring, so detect it and
                    // surface a clear message instead.
                    if uses_unsupported_syntax(&contents.contents) {
                        anyhow::bail!(
                            "Unsupported mermaid syntax: node metadata (`@{{ shape: … }}`) \
                             and the v11 shape set aren't supported by this renderer yet."
                        );
                    }
                    // Parse, attach semantic-category styles to the graph (only
                    // for flowcharts), then lay out and render — this is exactly
                    // what `render_with_options` does, with the category step
                    // inserted between parse and layout.
                    let mut parsed = mermaid_rs_renderer::parse_mermaid(&contents.contents)?;
                    if parsed.graph.kind == DiagramKind::Flowchart {
                        apply_categories(&mut parsed.graph, &category_styles);
                    }
                    let layout_config = mermaid_rs_renderer::LayoutConfig::default();
                    let layout =
                        mermaid_rs_renderer::compute_layout(&parsed.graph, &theme, &layout_config);
                    let svg_string = mermaid_rs_renderer::render_svg(&layout, &theme, &layout_config);
                    let scale = contents.scale as f32 / 100.0;
                    svg_renderer
                        .render_single_frame(svg_string.as_bytes(), scale)
                        .map_err(|error| anyhow::anyhow!("{error}"))
                })
                .await;
            let _ = render_image_clone.set(value);
            this.update(cx, |_, cx| {
                cx.notify();
            })
            .ok();
        });

        Self {
            render_image,
            fallback_image,
            _task: task,
        }
    }

    #[cfg(test)]
    fn new_for_test(
        render_image: Option<Arc<RenderImage>>,
        fallback_image: Option<Arc<RenderImage>>,
    ) -> Self {
        let result = Arc::new(OnceLock::new());
        if let Some(render_image) = render_image {
            let _ = result.set(Ok(render_image));
        }
        Self {
            render_image: result,
            fallback_image,
            _task: Task::ready(()),
        }
    }
}

fn parse_mermaid_info(info: &str) -> Option<u32> {
    let mut parts = info.split_whitespace();
    if parts.next()? != "mermaid" {
        return None;
    }

    Some(
        parts
            .next()
            .and_then(|scale| scale.parse().ok())
            .unwrap_or(100)
            .clamp(10, 500),
    )
}

pub(crate) fn extract_mermaid_diagrams(
    source: &str,
    events: &[(Range<usize>, MarkdownEvent)],
) -> BTreeMap<usize, ParsedMarkdownMermaidDiagram> {
    let mut mermaid_diagrams = BTreeMap::default();

    for (source_range, event) in events {
        let MarkdownEvent::Start(MarkdownTag::CodeBlock { kind, metadata }) = event else {
            continue;
        };
        let CodeBlockKind::FencedLang(info) = kind else {
            continue;
        };
        let Some(scale) = parse_mermaid_info(info.as_ref()) else {
            continue;
        };

        let contents = source[metadata.content_range.clone()]
            .strip_suffix('\n')
            .unwrap_or(&source[metadata.content_range.clone()])
            .to_string();
        mermaid_diagrams.insert(
            source_range.start,
            ParsedMarkdownMermaidDiagram {
                content_range: metadata.content_range.clone(),
                contents: ParsedMarkdownMermaidDiagramContents {
                    contents: contents.into(),
                    scale,
                },
            },
        );
    }

    mermaid_diagrams
}

pub(crate) fn render_mermaid_diagram(
    parsed: &ParsedMarkdownMermaidDiagram,
    mermaid_state: &MermaidState,
    style: &MarkdownStyle,
) -> AnyElement {
    let key = MermaidCacheKey {
        contents: parsed.contents.clone(),
        theme_key: mermaid_state.theme_key,
    };
    let cached = mermaid_state.cache.get(&key);
    let mut container = div().w_full();
    container.style().refine(&style.code_block);

    if let Some(result) = cached.and_then(|cached| cached.render_image.get()) {
        match result {
            Ok(render_image) => container
                .child(
                    div().w_full().child(
                        img(ImageSource::Render(render_image.clone()))
                            .max_w_full()
                            .with_fallback(|| {
                                div()
                                    .child(Label::new("Failed to load mermaid diagram"))
                                    .into_any_element()
                            }),
                    ),
                )
                .into_any_element(),
            Err(error) => container
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(
                                    Icon::new(IconName::Warning)
                                        .size(IconSize::Small)
                                        .color(Color::Warning),
                                )
                                .child(
                                    Label::new("Couldn't render this mermaid diagram")
                                        .color(Color::Muted),
                                ),
                        )
                        .child(
                            Label::new(error.to_string())
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(StyledText::new(parsed.contents.contents.clone())),
                )
                .into_any_element(),
        }
    } else if let Some(fallback) = cached.and_then(|cached| cached.fallback_image.as_ref()) {
        container
            .child(
                div()
                    .w_full()
                    .child(
                        img(ImageSource::Render(fallback.clone()))
                            .max_w_full()
                            .with_fallback(|| {
                                div()
                                    .child(Label::new("Failed to load mermaid diagram"))
                                    .into_any_element()
                            }),
                    )
                    .with_animation(
                        "mermaid-fallback-pulse",
                        Animation::new(Duration::from_secs(2))
                            .repeat()
                            .with_easing(pulsating_between(0.6, 1.0)),
                        |element, delta| element.opacity(delta),
                    ),
            )
            .into_any_element()
    } else {
        container
            .child(
                Label::new("Rendering mermaid diagram...")
                    .color(Color::Muted)
                    .with_animation(
                        "mermaid-loading-pulse",
                        Animation::new(Duration::from_secs(2))
                            .repeat()
                            .with_easing(pulsating_between(0.4, 0.8)),
                        |label, delta| label.alpha(delta),
                    ),
            )
            .into_any_element()
    }
}

/// True when the source uses mermaid syntax our renderer can't handle, so we
/// show a clear message rather than a garbled diagram. Currently this is the v11
/// node-metadata syntax (`A@{ shape: …, label: … }`): the renderer doesn't parse
/// it in flowcharts (only the kanban parser handles `@{`), so it silently dumps
/// the metadata into node labels instead of erroring.
fn uses_unsupported_syntax(source: &str) -> bool {
    source.contains("@{") && is_flowchart_source(source)
}

/// Whether the diagram declares a `flowchart`/`graph` type (skipping frontmatter
/// and `%%` directives).
fn is_flowchart_source(source: &str) -> bool {
    let mut in_frontmatter = false;
    for raw in source.lines() {
        let line = raw.trim();
        if line == "---" {
            in_frontmatter = !in_frontmatter;
            continue;
        }
        if in_frontmatter || line.is_empty() || line.starts_with("%%") {
            continue;
        }
        return line.starts_with("flowchart") || line.starts_with("graph");
    }
    false
}

/// Build a mermaid renderer theme from the active editor theme so diagrams blend
/// with the preview. The background is transparent so the diagram sits directly
/// on the preview surface rather than a stark white card.
fn mermaid_theme(cx: &App) -> mermaid_rs_renderer::Theme {
    let colors = cx.theme().colors();
    let bg = colors.editor_background;
    let fg = colors.text;

    // Filled shapes are "cards" that CONTRAST the page rather than blending into
    // it: light on dark themes, dark on light themes. We derive the card color by
    // compositing the foreground over the background, so it tracks any theme.
    // Text drawn on a card therefore uses the page background for contrast, while
    // text drawn on the page (titles) keeps the normal foreground color.
    let card = bg.blend(fg.opacity(0.85));
    let card_subtle = bg.blend(fg.opacity(0.7));
    let card_text = css_color(bg);
    let card_border = css_color(bg.blend(fg.opacity(0.45)));

    let mut theme = mermaid_rs_renderer::Theme::modern();
    theme.background = "transparent".to_string();
    theme.primary_color = css_color(card);
    theme.secondary_color = css_color(card);
    theme.tertiary_color = css_color(card_subtle);
    theme.primary_border_color = card_border.clone();
    // Used by the renderer for text on nodes, edge labels, and subgraph labels —
    // all of which sit on a contrasting card, so it reads against the page bg.
    theme.primary_text_color = card_text;
    // Text drawn directly on the page (e.g. diagram titles) stays foreground.
    theme.text_color = css_color(fg);
    theme.line_color = css_color(colors.text_muted);
    // Edge labels ("Yes"/"No") are little chips so their text matches node text.
    theme.edge_label_background = css_color(card);
    // Subgraph containers are a lighter card so their (page-bg) label stays legible.
    theme.cluster_background = css_color(card_subtle);
    theme.cluster_border = card_border.clone();
    theme.sequence_actor_fill = css_color(card);
    theme.sequence_actor_border = card_border.clone();
    theme.sequence_note_fill = css_color(card_subtle);
    theme.sequence_note_border = card_border;
    theme
}

/// An opaque identity for the colors a [`mermaid_theme`] and [`category_styles`]
/// produce, used in the diagram cache key so a theme change invalidates the
/// cached image.
fn theme_fingerprint(
    theme: &mermaid_rs_renderer::Theme,
    category_styles: &[(&'static str, NodeStyle)],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for field in [
        &theme.background,
        &theme.primary_color,
        &theme.secondary_color,
        &theme.tertiary_color,
        &theme.primary_border_color,
        &theme.primary_text_color,
        &theme.text_color,
        &theme.line_color,
        &theme.edge_label_background,
        &theme.cluster_background,
        &theme.cluster_border,
    ] {
        field.hash(&mut hasher);
    }
    for (name, style) in category_styles {
        name.hash(&mut hasher);
        style.fill.hash(&mut hasher);
        style.stroke.hash(&mut hasher);
        style.text_color.hash(&mut hasher);
        style.stroke_dasharray.hash(&mut hasher);
        style.stroke_width.map(f32::to_bits).hash(&mut hasher);
    }
    hasher.finish()
}

/// Format a color as a CSS `rgba(...)` string the SVG renderer understands.
fn css_color(color: Hsla) -> String {
    let rgba = color.to_rgb();
    format!(
        "rgba({}, {}, {}, {:.3})",
        (rgba.r * 255.0).round() as u8,
        (rgba.g * 255.0).round() as u8,
        (rgba.b * 255.0).round() as u8,
        rgba.a,
    )
}

/// A semantic node category. Flowchart nodes are auto-assigned a category by
/// shape and label/id keywords (an explicit `:::class`/`class` on a node wins),
/// and each category is styled with a distinct, theme-derived accent.
struct Category {
    /// Also the mermaid class name, so `node:::database` works too.
    name: &'static str,
    /// Fixed index into `theme.accents()` — stable, distinct hue per category.
    accent_index: u32,
    /// Shape that implies this category (e.g. a cylinder is a database).
    shape: Option<NodeShape>,
    /// Whole-word matches in a node's label/id that imply this category.
    keywords: &'static [&'static str],
    /// Dashed border (used to signal "outside the system").
    dashed: bool,
}

const CATEGORIES: &[Category] = &[
    Category {
        name: "database",
        accent_index: 0,
        shape: Some(NodeShape::Cylinder),
        keywords: &[
            "db",
            "database",
            "postgres",
            "postgresql",
            "mysql",
            "mongo",
            "mongodb",
            "sqlite",
            "sql",
        ],
        dashed: false,
    },
    Category {
        name: "queue",
        accent_index: 1,
        shape: None,
        keywords: &["queue", "kafka", "sqs", "rabbit", "rabbitmq", "jobs", "pubsub", "sns"],
        dashed: false,
    },
    Category {
        name: "cache",
        accent_index: 2,
        shape: None,
        keywords: &["cache", "redis", "memcache", "memcached"],
        dashed: false,
    },
    Category {
        name: "external",
        accent_index: 7,
        shape: None,
        keywords: &["external", "third-party", "thirdparty", "vendor", "webhook"],
        dashed: true,
    },
    Category {
        name: "api",
        accent_index: 6,
        shape: None,
        keywords: &["api", "gateway", "endpoint", "service", "svc"],
        dashed: false,
    },
];

/// Classify a flowchart node into a [`Category`] by shape first, then by a
/// whole-word keyword match on its label/id (so "api" doesn't match "capital").
fn classify(node: &Node) -> Option<&'static str> {
    if let Some(category) = CATEGORIES
        .iter()
        .find(|category| category.shape == Some(node.shape))
    {
        return Some(category.name);
    }

    let haystack = format!("{} {}", node.label, node.id).to_lowercase();
    let tokens: Vec<&str> = haystack
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|token| !token.is_empty())
        .collect();
    CATEGORIES
        .iter()
        .find(|category| {
            category
                .keywords
                .iter()
                .any(|keyword| tokens.iter().any(|token| token == keyword))
        })
        .map(|category| category.name)
}

/// Build the per-category node styles from the active theme. Each category is a
/// card (same contrasting lightness as a normal node, for legibility) tinted
/// with its accent hue, plus a vivid accent border — so categories are
/// distinguishable yet consistent with the theme.
fn category_styles(cx: &App) -> Vec<(&'static str, NodeStyle)> {
    let colors = cx.theme().colors();
    let accents = cx.theme().accents();
    let card = colors.editor_background.blend(colors.text.opacity(0.85));
    let card_text = css_color(colors.editor_background);

    CATEGORIES
        .iter()
        .map(|category| {
            let accent = accents.color_for_index(category.accent_index);
            // Keep the card's lightness (legibility) but take the accent's hue.
            let fill = Hsla {
                h: accent.h,
                s: accent.s.min(0.55),
                l: card.l,
                a: 1.0,
            };
            let style = NodeStyle {
                fill: Some(css_color(fill)),
                stroke: Some(css_color(accent)),
                stroke_width: Some(2.0),
                stroke_dasharray: category.dashed.then(|| "5 3".to_string()),
                text_color: Some(card_text.clone()),
                ..Default::default()
            };
            (category.name, style)
        })
        .collect()
}

/// Attach category styles to a parsed flowchart graph: register each category as
/// a `classDef` (without clobbering a user-defined one of the same name) and
/// auto-assign categories to nodes that don't already carry an explicit class.
fn apply_categories(graph: &mut Graph, category_styles: &[(&'static str, NodeStyle)]) {
    for (name, style) in category_styles {
        graph
            .class_defs
            .entry((*name).to_string())
            .or_insert_with(|| style.clone());
    }

    // Collect first (immutable borrow of nodes), then mutate node_classes.
    let detected: Vec<(String, &'static str)> = graph
        .nodes
        .iter()
        .filter(|(id, _)| graph.node_classes.get(*id).is_none_or(|classes| classes.is_empty()))
        .filter_map(|(id, node)| classify(node).map(|category| (id.clone(), category)))
        .collect();
    for (id, category) in detected {
        graph
            .node_classes
            .entry(id)
            .or_default()
            .push(category.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CATEGORIES, CachedMermaidDiagram, MermaidCacheKey, MermaidDiagramCache, MermaidState,
        NodeStyle, ParsedMarkdownMermaidDiagramContents, apply_categories, classify,
        extract_mermaid_diagrams, parse_mermaid_info, uses_unsupported_syntax,
    };
    use crate::{
        CodeBlockRenderer, CopyButtonVisibility, Markdown, MarkdownElement, MarkdownOptions,
        MarkdownStyle,
    };
    use collections::HashMap;
    use gpui::{Context, IntoElement, Render, RenderImage, TestAppContext, Window, size};
    use std::sync::Arc;
    use ui::prelude::*;

    fn ensure_theme_initialized(cx: &mut TestAppContext) {
        cx.update(|cx| {
            if !cx.has_global::<settings::SettingsStore>() {
                settings::init(cx);
            }
            if !cx.has_global::<theme::GlobalTheme>() {
                theme_settings::init(theme::LoadThemes::JustBase, cx);
            }
        });
    }

    fn render_markdown_with_options(
        markdown: &str,
        options: MarkdownOptions,
        cx: &mut TestAppContext,
    ) -> crate::RenderedText {
        struct TestWindow;

        impl Render for TestWindow {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }

        ensure_theme_initialized(cx);

        let (_, cx) = cx.add_window_view(|_, _| TestWindow);
        let markdown = cx.new(|cx| {
            Markdown::new_with_options(markdown.to_string().into(), None, None, options, cx)
        });
        cx.run_until_parked();
        let (rendered, _) = cx.draw(
            Default::default(),
            size(px(600.0), px(600.0)),
            |_window, _cx| {
                MarkdownElement::new(markdown, MarkdownStyle::default()).code_block_renderer(
                    CodeBlockRenderer::Default {
                        copy_button_visibility: CopyButtonVisibility::Hidden,
                        border: false,
                    },
                )
            },
        );
        rendered.text
    }

    fn mock_render_image(cx: &mut TestAppContext) -> Arc<RenderImage> {
        cx.update(|cx| {
            cx.svg_renderer()
                .render_single_frame(
                    br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>"#,
                    1.0,
                )
                .unwrap()
        })
    }

    fn mermaid_contents(contents: &str) -> MermaidCacheKey {
        MermaidCacheKey {
            contents: ParsedMarkdownMermaidDiagramContents {
                contents: contents.to_string().into(),
                scale: 100,
            },
            theme_key: 0,
        }
    }

    fn mermaid_sequence(diagrams: &[&str]) -> Vec<MermaidCacheKey> {
        diagrams
            .iter()
            .map(|diagram| mermaid_contents(diagram))
            .collect()
    }

    fn mermaid_fallback(
        new_diagram: &str,
        new_full_order: &[MermaidCacheKey],
        old_full_order: &[MermaidCacheKey],
        cache: &MermaidDiagramCache,
    ) -> Option<Arc<RenderImage>> {
        let new_content = mermaid_contents(new_diagram);
        let idx = new_full_order
            .iter()
            .position(|diagram| diagram == &new_content)?;
        MermaidState::get_fallback_image(idx, old_full_order, new_full_order.len(), cache)
    }

    #[test]
    fn test_uses_unsupported_syntax() {
        // v11 node-metadata in a flowchart: unsupported (renders garbled).
        assert!(uses_unsupported_syntax(
            "flowchart TB\n  A@{ shape: rect, label: \"X\" }"
        ));
        assert!(uses_unsupported_syntax("graph LR\n  A@{ shape: cyl }"));
        // Frontmatter before the flowchart declaration is skipped.
        assert!(uses_unsupported_syntax(
            "---\ntitle: x\n---\nflowchart TB\n  A@{ shape: doc }"
        ));
        // Classic flowchart shape syntax is supported.
        assert!(!uses_unsupported_syntax(
            "flowchart TB\n  A[Start] --> B{Decision} --> C[(DB)]"
        ));
        // `@{` outside a flowchart (e.g. kanban, which the renderer handles) is
        // not flagged.
        assert!(!uses_unsupported_syntax(
            "kanban\n  Todo\n    A@{ assigned: bob }"
        ));
    }

    #[test]
    fn test_classify_false_positives() {
        let parsed =
            mermaid_rs_renderer::parse_mermaid("flowchart TB\n  X[Capital city]\n  Y[Adblock]")
                .unwrap();
        // Whole-word matching: "capital" is not "api", "adblock" is not "db".
        assert_eq!(classify(&parsed.graph.nodes["X"]), None);
        assert_eq!(classify(&parsed.graph.nodes["Y"]), None);
    }

    #[test]
    fn test_apply_categories() {
        let parsed = mermaid_rs_renderer::parse_mermaid(
            "flowchart TB\n  \
             classDef database fill:#abcdef\n  \
             A[(Users DB)]\n  \
             B[Redis cache]\n  \
             C[Payment API]\n  \
             D[Start]\n  \
             E[Webhook]:::external\n  \
             A-->B-->C-->D-->E",
        )
        .unwrap();
        let mut graph = parsed.graph;
        let styles: Vec<(&'static str, NodeStyle)> = CATEGORIES
            .iter()
            .map(|category| (category.name, NodeStyle::default()))
            .collect();

        apply_categories(&mut graph, &styles);

        // Auto-detected categories (shape / keyword).
        assert_eq!(graph.node_classes.get("A").map(Vec::as_slice), Some(&["database".to_string()][..]));
        assert_eq!(graph.node_classes.get("B").map(Vec::as_slice), Some(&["cache".to_string()][..]));
        assert_eq!(graph.node_classes.get("C").map(Vec::as_slice), Some(&["api".to_string()][..]));
        // Unclassifiable node gets nothing.
        assert!(graph.node_classes.get("D").map_or(true, |c| c.is_empty()));
        // Explicit `:::external` is respected and not duplicated.
        assert_eq!(graph.node_classes.get("E").map(Vec::as_slice), Some(&["external".to_string()][..]));
        // All categories are registered as classDefs...
        assert!(graph.class_defs.contains_key("queue"));
        // ...but a user-defined classDef is not clobbered.
        assert_eq!(graph.class_defs["database"].fill.as_deref(), Some("#abcdef"));
    }

    #[test]
    fn test_parse_mermaid_info() {
        assert_eq!(parse_mermaid_info("mermaid"), Some(100));
        assert_eq!(parse_mermaid_info("mermaid 150"), Some(150));
        assert_eq!(parse_mermaid_info("mermaid 5"), Some(10));
        assert_eq!(parse_mermaid_info("mermaid 999"), Some(500));
        assert_eq!(parse_mermaid_info("rust"), None);
    }

    #[test]
    fn test_extract_mermaid_diagrams_parses_scale() {
        let markdown = "```mermaid 150\ngraph TD;\n```\n\n```rust\nfn main() {}\n```";
        let events = crate::parser::parse_markdown_with_options(markdown, false, false).events;
        let diagrams = extract_mermaid_diagrams(markdown, &events);

        assert_eq!(diagrams.len(), 1);
        let diagram = diagrams.values().next().unwrap();
        assert_eq!(diagram.contents.contents, "graph TD;");
        assert_eq!(diagram.contents.scale, 150);
    }

    #[gpui::test]
    fn test_mermaid_fallback_on_edit(cx: &mut TestAppContext) {
        let old_full_order = mermaid_sequence(&["graph A", "graph B", "graph C"]);
        let new_full_order = mermaid_sequence(&["graph A", "graph B modified", "graph C"]);

        let svg_b = mock_render_image(cx);

        let mut cache: MermaidDiagramCache = HashMap::default();
        cache.insert(
            mermaid_contents("graph A"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                Some(mock_render_image(cx)),
                None,
            )),
        );
        cache.insert(
            mermaid_contents("graph B"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                Some(svg_b.clone()),
                None,
            )),
        );
        cache.insert(
            mermaid_contents("graph C"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                Some(mock_render_image(cx)),
                None,
            )),
        );

        let fallback =
            mermaid_fallback("graph B modified", &new_full_order, &old_full_order, &cache);

        assert_eq!(fallback.as_ref().map(|image| image.id), Some(svg_b.id));
    }

    #[gpui::test]
    fn test_mermaid_no_fallback_on_add_in_middle(cx: &mut TestAppContext) {
        let old_full_order = mermaid_sequence(&["graph A", "graph C"]);
        let new_full_order = mermaid_sequence(&["graph A", "graph NEW", "graph C"]);

        let mut cache: MermaidDiagramCache = HashMap::default();
        cache.insert(
            mermaid_contents("graph A"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                Some(mock_render_image(cx)),
                None,
            )),
        );
        cache.insert(
            mermaid_contents("graph C"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                Some(mock_render_image(cx)),
                None,
            )),
        );

        let fallback = mermaid_fallback("graph NEW", &new_full_order, &old_full_order, &cache);

        assert!(fallback.is_none());
    }

    #[gpui::test]
    fn test_mermaid_fallback_chains_on_rapid_edits(cx: &mut TestAppContext) {
        let old_full_order = mermaid_sequence(&["graph A", "graph B modified", "graph C"]);
        let new_full_order = mermaid_sequence(&["graph A", "graph B modified again", "graph C"]);

        let original_svg = mock_render_image(cx);

        let mut cache: MermaidDiagramCache = HashMap::default();
        cache.insert(
            mermaid_contents("graph A"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                Some(mock_render_image(cx)),
                None,
            )),
        );
        cache.insert(
            mermaid_contents("graph B modified"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                None,
                Some(original_svg.clone()),
            )),
        );
        cache.insert(
            mermaid_contents("graph C"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                Some(mock_render_image(cx)),
                None,
            )),
        );

        let fallback = mermaid_fallback(
            "graph B modified again",
            &new_full_order,
            &old_full_order,
            &cache,
        );

        assert_eq!(
            fallback.as_ref().map(|image| image.id),
            Some(original_svg.id)
        );
    }

    #[gpui::test]
    fn test_mermaid_fallback_with_duplicate_blocks_edit_second(cx: &mut TestAppContext) {
        let old_full_order = mermaid_sequence(&["graph A", "graph A", "graph B"]);
        let new_full_order = mermaid_sequence(&["graph A", "graph A edited", "graph B"]);

        let svg_a = mock_render_image(cx);

        let mut cache: MermaidDiagramCache = HashMap::default();
        cache.insert(
            mermaid_contents("graph A"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                Some(svg_a.clone()),
                None,
            )),
        );
        cache.insert(
            mermaid_contents("graph B"),
            Arc::new(CachedMermaidDiagram::new_for_test(
                Some(mock_render_image(cx)),
                None,
            )),
        );

        let fallback = mermaid_fallback("graph A edited", &new_full_order, &old_full_order, &cache);

        assert_eq!(fallback.as_ref().map(|image| image.id), Some(svg_a.id));
    }

    #[gpui::test]
    fn test_mermaid_rendering_replaces_code_block_text(cx: &mut TestAppContext) {
        let rendered = render_markdown_with_options(
            "```mermaid\ngraph TD;\n```",
            MarkdownOptions {
                render_mermaid_diagrams: true,
                ..Default::default()
            },
            cx,
        );

        let text = rendered
            .lines
            .iter()
            .map(|line| line.layout.wrapped_text())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!text.contains("graph TD;"));
    }

    #[gpui::test]
    fn test_mermaid_source_anchor_maps_inside_block(cx: &mut TestAppContext) {
        struct TestWindow;

        impl Render for TestWindow {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }

        ensure_theme_initialized(cx);

        let (_, cx) = cx.add_window_view(|_, _| TestWindow);
        let markdown = cx.new(|cx| {
            Markdown::new_with_options(
                "```mermaid\ngraph TD;\n```".into(),
                None,
                None,
                MarkdownOptions {
                    render_mermaid_diagrams: true,
                    ..Default::default()
                },
                cx,
            )
        });
        cx.run_until_parked();
        let render_image = mock_render_image(cx);
        markdown.update(cx, |markdown, _| {
            let contents = markdown
                .parsed_markdown
                .mermaid_diagrams
                .values()
                .next()
                .unwrap()
                .contents
                .clone();
            let key = MermaidCacheKey {
                contents,
                theme_key: markdown.mermaid_state.theme_key,
            };
            markdown.mermaid_state.cache.insert(
                key.clone(),
                Arc::new(CachedMermaidDiagram::new_for_test(Some(render_image), None)),
            );
            markdown.mermaid_state.order = vec![key];
        });

        let (rendered, _) = cx.draw(
            Default::default(),
            size(px(600.0), px(600.0)),
            |_window, _cx| {
                MarkdownElement::new(markdown.clone(), MarkdownStyle::default())
                    .code_block_renderer(CodeBlockRenderer::Default {
                        copy_button_visibility: CopyButtonVisibility::Hidden,
                        border: false,
                    })
            },
        );

        let mermaid_diagram = markdown.update(cx, |markdown, _| {
            markdown
                .parsed_markdown
                .mermaid_diagrams
                .values()
                .next()
                .unwrap()
                .clone()
        });
        assert!(
            rendered
                .text
                .position_for_source_index(mermaid_diagram.content_range.start)
                .is_some()
        );
        assert!(
            rendered
                .text
                .position_for_source_index(mermaid_diagram.content_range.end.saturating_sub(1))
                .is_some()
        );
    }
}
