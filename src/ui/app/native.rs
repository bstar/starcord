//! Focus Signal: one shared KIT workspace recipe for all five CORD panes.
//!
//! Existing widgets measure content and record hits in the pane engine's cells.
//! Native surfaces retain that grid for content, while KIT owns physical chrome,
//! controls, fonts, rasterization and frontend recovery. No second message model.
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use starkit::chrome::header::Word as _;
use starkit::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use starkit::native_surface::{
    style::{hex, Palette, Style as Visual},
    PixelRect as R, Primitive, Surface,
};
use starkit::ratatui::{buffer::Buffer, layout::Rect, style::Modifier};
use starkit::terminal_graphics::{
    assets::Thumbnailer,
    capabilities::Capabilities,
    placement::Placement as PixelPlacement,
    protocol::{Component, ImageScale, Input, Scene, ServerMessage, Span, Viewport},
    session::Controller,
};

use super::{App, NoteLevel, FRAME};
use crate::discord::media::{Decoded, MediaKey};
use crate::discord::Command;
use crate::ui::panels::{self, chat::media::Placement, ModuleId, Word, COLUMN};

/// Only frame-local presentation evidence. Domain state stays in App and the core.
#[derive(Default)]
pub(super) struct Paint {
    pub native: bool,
    pub base: Option<Buffer>,
    pub pictures: Vec<Placement>,
    pub overlay_pictures: Vec<Placement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Pane(ModuleId),
    Word(ModuleId, Word),
}

struct ImageSlot {
    encoder: Thumbnailer,
    pending: String,
    ready: Option<(String, String)>,
}

pub struct Native {
    app: App,
    cells: bool,
    native_skins: bool,
    style: Visual,
    viewport: Viewport,
    canvas: Viewport,
    placements: Vec<PixelPlacement>,
    hits: Vec<(R, Action)>,
    images: HashMap<MediaKey, ImageSlot>,
    image_pool: Vec<ImageSlot>,
    revision: u64,
    interaction: u64,
    toggle_presentation: bool,
    geometry: Option<Geometry>,
}

#[derive(PartialEq, Eq)]
struct Geometry {
    generation: u64,
    panes: [Rect; 5],
    focus: ModuleId,
    channel: Option<crate::discord::snowflake::ChannelId>,
    view_version: u64,
    modal: u16,
    chat: Vec<(Rect, crate::ui::panels::chat::Hit)>,
}

pub fn palette(theme: &starkit::theme::Theme) -> Palette {
    let mut p = match theme.id.as_str() {
        "winamp-classic" => Palette::default(),
        "winamp-silver" => starkit::native_surface::style::SILVER_PALETTE,
        _ => Palette::from_theme(theme),
    };
    // Same graphical Kanagawa interpretation as AMP and FOLD.
    if theme.name == "Kanagawa" {
        p.bg = [theme.header_bg.r, theme.header_bg.g, theme.header_bg.b];
        let inset = theme.bg.mix(starkit::theme::BLACK, 0.12);
        p.inset_bg = Some([inset.r, inset.g, inset.b]);
    }
    p
}

impl Native {
    pub fn new(mut app: App) -> Self {
        let style = Visual::new(palette(&app.look.theme));
        app.set_terminal_focus(false);
        app.native_paint = Some(Paint::default());
        Self {
            app,
            cells: false,
            native_skins: true,
            style,
            viewport: Viewport::default(),
            canvas: Viewport::default(),
            placements: vec![],
            hits: vec![],
            images: HashMap::new(),
            image_pool: vec![],
            revision: 0,
            interaction: 0,
            toggle_presentation: false,
            geometry: None,
        }
    }

    fn visual(&self) -> Visual {
        self.style.clone().with_native_skins(self.native_skins)
    }

    fn pane(&mut self, scene: &mut Scene, visual: &Visual, module: ModuleId, rect: Rect) {
        let target = pixels(rect, scene.viewport);
        if target.width == 0 || target.height == 0 {
            return;
        }
        let active = self.app.layout.focus() == module && !self.app.over.open();
        let mut panel = visual.workspace_panel(target.width, target.height, "");
        let header_bottom = pixels(
            Rect::new(rect.x, rect.y, rect.width, 2.min(rect.height)),
            scene.viewport,
        )
        .height;
        let selected = hex(visual.palette.selected);
        panel.fill(
            R::new(0, 0, target.width, header_bottom),
            if active {
                &selected
            } else {
                &visual.workspace.header
            },
            0,
        );
        panel.fill(
            R::new(0, header_bottom.saturating_sub(1), target.width, 1),
            &visual.workspace.line,
            0,
        );

        let mut right = target.width.saturating_sub(visual.metrics.padding);
        for word in panels::words(module).into_iter().rev() {
            let label = word.word();
            let width = (label.chars().count() as u16 * 7 + 20).max(28);
            if right < width + 150 {
                continue;
            }
            right -= width;
            let face = R::new(
                right,
                header_bottom.saturating_sub(28) / 2,
                width,
                28.min(header_bottom),
            );
            visual.tool(
                &mut panel,
                face,
                &label,
                &format!("word:{module:?}:{word:?}"),
                false,
            );
            self.hits.push((
                R::new(
                    target.x + face.x,
                    target.y + face.y,
                    face.width,
                    face.height,
                ),
                Action::Word(module, word),
            ));
            right = right.saturating_sub(visual.metrics.gap);
        }
        let (name, detail) = self.app.module_title(module);
        let name = name.to_uppercase();
        let arrow = if module.is_list() {
            if self.app.layout.is_expanded(module) {
                "▾ "
            } else {
                "▸ "
            }
        } else {
            ""
        };
        let name = if module == ModuleId::Servers {
            format!("STAR/CORD  ·  {arrow}{name}")
        } else {
            format!("{arrow}{name}")
        };
        let title_width = if module == ModuleId::Servers {
            220
        } else {
            130
        }
        .min(right.saturating_sub(24));
        let title_y = header_bottom.saturating_sub(24) / 2;
        let accent = hex(visual.palette.accent);
        panel.text(
            R::new(
                12.min(target.width),
                title_y,
                title_width,
                24.min(header_bottom),
            ),
            name,
            if active { &accent } else { &visual.colors.ink },
            12,
            true,
        );
        if let Some(detail) = detail {
            let x = 24 + title_width;
            if right > x + 24 {
                panel.text(
                    R::new(x, title_y, right - x, 24.min(header_bottom)),
                    detail,
                    &visual.colors.dim,
                    11,
                    false,
                );
            }
        }
        self.hits.push((
            R::new(target.x, target.y, right, header_bottom),
            Action::Pane(module),
        ));

        // Leave a small shared seam at the foot of every module.
        let gap = visual
            .metrics
            .gap
            .min(target.height.saturating_sub(header_bottom));
        panel.fill(
            R::new(0, target.height - gap, target.width, gap),
            &hex(visual.palette.bg),
            0,
        );
        if active {
            // KIT supplies the focus shape; the second inset makes the chosen
            // two-pixel Focus Signal outline without application RGB constants.
            visual.focus(&mut panel, R::new(0, 0, target.width, target.height - gap));
            if target.width > 2 && target.height > gap + 2 {
                visual.focus(
                    &mut panel,
                    R::new(1, 1, target.width - 2, target.height - gap - 2),
                );
            }
        }
        visual
            .mount(scene, panel, target.x, target.y)
            .expect("bounded pane from KIT layout");
        if let Some(Component::Surface { rect: source, .. }) = scene.components.last_mut() {
            *source = rect.into();
        }
        scene.placements.last_mut().unwrap().source = rect.into();
    }

    fn media(
        &mut self,
        scene: &mut Scene,
        places: &[Placement],
        visible: &mut HashSet<MediaKey>,
        budget: &mut usize,
    ) {
        let mut wanted = HashSet::new();
        for place in places {
            if wanted.len() >= 8 {
                break;
            }
            if !place.clipped
                && place.rect.intersection(place.clip) == place.rect
                && self.app.chat.media.decoded(&place.key).is_some()
            {
                wanted.insert(place.key.clone());
            }
        }
        self.recycle_images(&wanted);
        for place in places {
            // Partially scrolled pictures retain the existing clipped-cell paint.
            if place.clipped || place.rect.intersection(place.clip) != place.rect {
                continue;
            }
            if visible.len() >= 8 && !visible.contains(&place.key) {
                continue;
            }
            if !wanted.contains(&place.key) {
                continue;
            }
            let Some(decoded) = self.app.chat.media.decoded(&place.key) else {
                continue;
            };
            let frame = self.app.chat.anim.frame_of(&place.key);
            let image = match &*decoded {
                Decoded::Still { image, .. } => image,
                Decoded::Animated { frames, .. } if !frames.is_empty() => {
                    &frames[frame % frames.len()]
                }
                _ => continue,
            };
            visible.insert(place.key.clone());
            let id = format!(
                "cord-{:x}",
                starkit::graphics::ImageId::of(&(
                    &place.key,
                    frame,
                    image.as_ptr() as usize,
                    image.dimensions()
                ))
                .0
            );
            let pool = &mut self.image_pool;
            let slot = self.images.entry(place.key.clone()).or_insert_with(|| {
                pool.pop().unwrap_or_else(|| ImageSlot {
                    encoder: Thumbnailer::default(),
                    pending: String::new(),
                    ready: None,
                })
            });
            for ready in slot.encoder.output.try_iter() {
                if ready.0 == slot.pending {
                    slot.ready = Some(ready);
                }
            }
            if slot.pending != id {
                slot.encoder.request(id.clone(), image.clone());
                slot.pending = id;
            }
            if let Some((id, png)) = &slot.ready {
                if png.len() > *budget {
                    continue;
                }
                *budget -= png.len();
                scene.components.push(Component::Image {
                    rect: place.rect.into(),
                    id: id.clone(),
                    png: Some(png.clone()),
                    scale: ImageScale::Smooth,
                    zoom: 100,
                });
            }
        }
    }

    fn recycle_images(&mut self, visible: &HashSet<MediaKey>) {
        let expired: Vec<_> = self
            .images
            .keys()
            .filter(|key| !visible.contains(*key))
            .cloned()
            .collect();
        for key in expired {
            let mut slot = self.images.remove(&key).unwrap();
            slot.pending.clear();
            slot.ready = None;
            self.image_pool.push(slot);
        }
        debug_assert!(self.images.len() + self.image_pool.len() <= 8);
    }
}

impl Controller for Native {
    fn tick(&mut self) {
        self.app.last_frame = Instant::now();
        self.app.tick();
    }
    fn frame_interval(&self) -> std::time::Duration {
        FRAME
    }
    fn closed(&self) -> bool {
        self.app.quit
    }
    fn shutdown(&mut self) {
        self.app.core.send(Command::SaveSession);
        self.app.core.send(Command::Shutdown);
    }
    fn attached(&mut self) {
        self.app.set_terminal_focus(true);
    }
    fn detached(&mut self) {
        self.app.set_terminal_focus(false);
        self.app.layout.drag = None;
    }
    fn capabilities(&mut self, capabilities: Capabilities) {
        self.native_skins = capabilities.native_skins;
        self.cells = capabilities
            .cell_presentation
            .unwrap_or(!capabilities.native_surfaces);
    }
    fn supports_presentation_switch(&self) -> bool {
        true
    }
    fn effects(&mut self) -> Vec<ServerMessage> {
        if std::mem::take(&mut self.toggle_presentation) {
            vec![ServerMessage::TogglePresentation]
        } else {
            vec![]
        }
    }
    fn presentation(&mut self, cells: bool) {
        self.cells = cells;
        self.hits.clear();
        self.placements.clear();
        self.recycle_images(&HashSet::new());
        self.app.chat.cache.clear();
    }

    fn scene(&mut self, viewport: Viewport) -> Scene {
        // KIT's frontend owns the actual image protocol in both presentations.
        // The controller's cell paint is only its bounded media fallback.
        let fallback = if starkit::graphics::Mode::parse(&self.app.cfg.ui.graphics)
            == starkit::graphics::Mode::Off
        {
            starkit::graphics::Mode::Off
        } else {
            starkit::graphics::Mode::Blocks
        };
        if self.app.look.graphics.mode() != fallback {
            self.app.look.graphics.set_mode(fallback);
        }
        self.viewport = viewport;
        let palette = palette(&self.app.look.theme);
        if self.style.palette != palette {
            self.style = Visual::new(palette);
        }
        // KIT's native text uses physical pixels rather than terminal font
        // metrics. Reuse the same pane solver on a matching content grid.
        let viewport = if self.cells {
            viewport
        } else {
            native_viewport(viewport)
        };
        self.canvas = viewport;
        self.app.native_paint.as_mut().unwrap().native = !self.cells;
        let area = Rect::new(0, 0, viewport.columns, viewport.rows);
        let mut buffer = Buffer::empty(area);
        let visual = self.visual();
        let theme = if self.cells {
            self.app.look.theme.clone()
        } else {
            self.app.look.theme.workspace(&visual)
        };
        let original = std::mem::replace(&mut self.app.look.theme, theme.clone());
        self.app.draw(area, &mut buffer);
        self.app.look.theme = original;
        self.revision = self.revision.wrapping_add(1);
        let over = &self.app.over;
        let modal = u16::from(over.help)
            | (u16::from(over.confirm.is_some()) << 1)
            | (u16::from(over.quick.is_some()) << 2)
            | (u16::from(over.settings.is_some()) << 3)
            | (u16::from(over.picker.is_some()) << 4)
            | (u16::from(over.viewer.is_some()) << 5)
            | (u16::from(over.search.is_some()) << 6)
            | (u16::from(over.attach.is_some()) << 7)
            | (u16::from(over.menu.is_some()) << 8);
        let geometry = Geometry {
            generation: viewport.generation,
            panes: COLUMN.map(|id| {
                self.app
                    .layout
                    .last
                    .as_ref()
                    .map_or(Rect::default(), |r| r.rect_of(id))
            }),
            focus: self.app.layout.focus(),
            channel: self.app.nav.channel,
            view_version: self.app.view.version,
            modal,
            chat: self.app.chat.hit_regions().to_vec(),
        };
        if self.geometry.as_ref() != Some(&geometry) {
            self.geometry = Some(geometry);
            self.interaction = self.interaction.wrapping_add(1);
        }
        self.hits.clear();
        self.placements.clear();
        if self.cells {
            let mut scene = Scene::from_buffer(&buffer, viewport, self.revision);
            scene.accent = self.app.look.theme.accent.to_hex();
            scene.border = self.app.look.theme.border.to_hex();
            scene.interaction = self.interaction;
            return scene;
        }
        let mut scene = visual.scene(viewport);
        scene.revision = self.revision;
        scene.interaction = self.interaction;
        // The scroll target remains the same module during ordinary animation.
        scene.scroll_interaction = Some(
            starkit::graphics::ImageId::of(&(
                viewport.generation,
                self.app.nav.channel,
                self.app.layout.expanded(),
                self.app.layout.focus(),
                modal,
            ))
            .0,
        );
        let paint = self
            .app
            .native_paint
            .as_mut()
            .expect("native frame capture");
        let base = if self.app.over.open() {
            paint.base.take().unwrap_or_else(|| buffer.clone())
        } else {
            buffer.clone()
        };
        let pictures = std::mem::take(&mut paint.pictures);
        let overlay_pictures = std::mem::take(&mut paint.overlay_pictures);

        if self.app.login.is_some() || self.app.layout.last.is_none() {
            mount_buffer(&mut scene, &visual, &buffer, area, &theme);
            return scene;
        }
        let regions = self.app.layout.last.clone().unwrap();
        for module in COLUMN {
            let rect = regions.rect_of(module);
            self.pane(&mut scene, &visual, module, rect);
            mount_buffer(&mut scene, &visual, &base, panels::body(rect), &theme);
        }
        mount_buffer(&mut scene, &visual, &base, regions.status, &theme);
        scene.placements.push(PixelPlacement::new(
            regions.status.into(),
            pixels(regions.status, viewport),
        ));
        let mut visible = HashSet::new();
        let mut budget = 8_000_000;
        if !self.app.over.open() {
            self.media(&mut scene, &pictures, &mut visible, &mut budget);
        }
        if self.app.over.open() {
            if let Some(rect) = changed_rect(&base, &buffer) {
                let target = pixels(rect, viewport);
                let mut modal = visual.modal_panel(target.width, target.height, "");
                let title: String = (rect.x..rect.right())
                    .map(|x| buffer[(x, rect.y)].symbol())
                    .filter(|s| !s.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c)))
                    .collect();
                if target.width > 24 {
                    modal.text(
                        R::new(12, 0, target.width - 24, 20.min(target.height)),
                        title.trim(),
                        &visual.colors.ink,
                        11,
                        true,
                    );
                }
                scene.components.push(Component::Menu { rect: rect.into() });
                visual
                    .mount(&mut scene, modal, target.x, target.y)
                    .expect("bounded overlay");
                if let Some(Component::Surface { rect: source, .. }) = scene.components.last_mut() {
                    *source = rect.into();
                }
                scene.placements.last_mut().unwrap().source = rect.into();
                let inner = Rect::new(
                    rect.x + 1,
                    rect.y + 1,
                    rect.width.saturating_sub(2),
                    rect.height.saturating_sub(2),
                );
                mount_buffer(&mut scene, &visual, &buffer, inner, &theme);
            }
        }
        if self.app.over.open() {
            self.media(&mut scene, &overlay_pictures, &mut visible, &mut budget);
        }
        self.recycle_images(&visible);
        // Exactly one KIT placement per pane, plus status and the active modal.
        // Content, images and pointer projection share these solved rectangles.
        self.placements = scene.placements.clone();
        scene
    }

    fn input(&mut self, input: Input) {
        match input {
            Input::Key { code, modifiers } => {
                self.app.set_terminal_focus(true);
                if code == "f:9" && modifiers == 0 {
                    self.toggle_presentation = true;
                    return;
                }
                if let Some(code) = key_code(&code) {
                    self.app.key(KeyEvent::new(
                        code,
                        KeyModifiers::from_bits_truncate(modifiers),
                    ));
                }
            }
            Input::Paste { text } => {
                self.app.set_terminal_focus(true);
                self.app.paste(&text);
            }
            Input::Pointer {
                action,
                button,
                x,
                y,
                pixel,
                modifiers,
            } => {
                if action == "down" || action.starts_with("scroll_") {
                    self.app.set_terminal_focus(true);
                }
                let [mut px, mut py] = pixel.unwrap_or_else(|| {
                    [
                        (u32::from(x) * 2 + 1) * self.viewport.width
                            / (u32::from(self.viewport.columns) * 2),
                        (u32::from(y) * 2 + 1) * self.viewport.height
                            / (u32::from(self.viewport.rows) * 2),
                    ]
                });
                if px >= self.viewport.width || py >= self.viewport.height {
                    if action != "drag" && action != "up" {
                        return;
                    }
                    px = px.min(self.viewport.width - 1);
                    py = py.min(self.viewport.height - 1);
                }
                let (x, y) = if self.cells {
                    (x, y)
                } else {
                    let fallback = (
                        (px * u32::from(self.canvas.columns) / self.canvas.width)
                            .min(u32::from(self.canvas.columns - 1)) as u16,
                        (py * u32::from(self.canvas.rows) / self.canvas.height)
                            .min(u32::from(self.canvas.rows - 1)) as u16,
                    );
                    self.placements
                        .iter()
                        .rev()
                        .find_map(|p| p.pointer_pixels(px, py, false))
                        .unwrap_or(fallback)
                };
                if !self.cells
                    && !self.app.over.open()
                    && self.app.login.is_none()
                    && action == "down"
                    && button == 0
                {
                    if let Some((_, hit)) = self
                        .hits
                        .iter()
                        .rev()
                        .find(|(r, _)| r.contains(px as u16, py as u16))
                    {
                        match *hit {
                            Action::Pane(module)
                                if module.is_list() && self.app.layout.is_expanded(module) =>
                            {
                                self.app.layout.collapse(ModuleId::Conversation)
                            }
                            Action::Pane(module) => self.app.focus_module(module),
                            Action::Word(module, word) => self.app.word_click(module, word),
                        }
                        return;
                    }
                }
                if let Some(kind) = mouse_kind(&action, button) {
                    self.app.handle_mouse(
                        MouseEvent {
                            kind,
                            column: x,
                            row: y,
                            modifiers: KeyModifiers::from_bits_truncate(modifiers),
                        },
                        Rect::new(0, 0, self.canvas.columns, self.canvas.rows),
                    );
                }
            }
            Input::CancelPointer => {
                self.app.layout.drag = None;
                // KIT emits this on focus loss. Resume read acknowledgements only
                // after real input, never while a detached/background app waits.
                self.app.set_terminal_focus(false);
            }
            Input::Resize { .. } => {
                self.app.chat.cache.clear();
                self.hits.clear();
            }
            Input::Notice { message } if message == "starcord:focus-gained" => {
                self.app.set_terminal_focus(true);
            }
            Input::Notice { message } => {
                self.app.note = Some((message, NoteLevel::Warning, Instant::now()))
            }
            Input::Detach => self.detached(),
            _ => {}
        }
    }
}

/// Exact shared grid edges, including fractional terminal cell dimensions.
fn pixels(rect: Rect, v: Viewport) -> R {
    let x = u32::from(rect.x) * v.width / u32::from(v.columns);
    let y = u32::from(rect.y) * v.height / u32::from(v.rows);
    let right = u32::from(rect.right()) * v.width / u32::from(v.columns);
    let bottom = u32::from(rect.bottom()) * v.height / u32::from(v.rows);
    R::new(x as u16, y as u16, (right - x) as u16, (bottom - y) as u16)
}

fn native_viewport(terminal: Viewport) -> Viewport {
    let mut columns = (terminal.width / 8).max(1);
    let mut rows = (terminal.height / 18).max(1);
    while columns * rows > starkit::terminal_graphics::protocol::MAX_CELLS as u32 {
        columns = columns.div_ceil(2);
        rows = rows.div_ceil(2);
    }
    Viewport {
        columns: columns as u16,
        rows: rows as u16,
        ..terminal
    }
}

fn changed_rect(before: &Buffer, after: &Buffer) -> Option<Rect> {
    let mut rect: Option<Rect> = None;
    for y in after.area.y..after.area.bottom() {
        for x in after.area.x..after.area.right() {
            if before[(x, y)] != after[(x, y)] {
                let cell = Rect::new(x, y, 1, 1);
                rect = Some(rect.map_or(cell, |r| r.union(cell)));
            }
        }
    }
    rect
}

/// Convert already measured content without changing any text or pointer columns.
/// A row per surface bounds primitive counts even for dense, styled messages.
fn mount_buffer(
    scene: &mut Scene,
    visual: &Visual,
    buffer: &Buffer,
    rect: Rect,
    theme: &crate::ui::theme::Theme,
) {
    let source = Scene::from_buffer(buffer, scene.viewport, 0);
    let cw = (scene.viewport.width / u32::from(scene.viewport.columns)).max(1) as u16;
    let ch = (scene.viewport.height / u32::from(scene.viewport.rows)).max(1) as u16;
    for y in rect.y..rect.bottom() {
        // Bounded chunks protect Surface's primitive limit even if every
        // character in a hostile message changes markup style.
        let mut x = rect.x;
        while x < rect.right() {
            let mut width = 512.min(rect.right() - x);
            if x + width < rect.right()
                && starkit::wrap::width_of(buffer[(x + width - 1, y)].symbol()) > 1
            {
                width -= 1;
            }
            let row = Rect::new(x, y, width, 1).intersection(buffer.area);
            x += width;
            if row.is_empty() {
                continue;
            }
            let mut surface =
                Surface::new(row.width * cw, ch, visual.workspace.well.clone()).with_ui_text();
            surface.cell_size = Some([cw, ch]);
            for span in source.spans.iter().filter(|span| span.y == y) {
                append_span(&mut surface, span, row, (cw, ch), visual, theme);
            }
            scene.components.push(Component::Surface {
                rect: row.into(),
                surface,
            });
        }
    }
}

fn append_span(
    surface: &mut Surface,
    span: &Span,
    row: Rect,
    cell: (u16, u16),
    visual: &Visual,
    theme: &crate::ui::theme::Theme,
) {
    let mut x = span.x;
    let mut text = String::new();
    let mut start = None;
    let mut end = 0;
    for (_, cluster) in starkit::wrap::clusters(&span.text) {
        let width = starkit::wrap::width_of(cluster);
        if x >= row.x && x.saturating_add(width) <= row.right() {
            start.get_or_insert(x);
            text.push_str(cluster);
            end = x.saturating_add(width);
        }
        x = x.saturating_add(width);
    }
    let Some(start) = start else {
        return;
    };
    let r = R::new((start - row.x) * cell.0, 0, (end - start) * cell.0, cell.1);
    // Neutral content uses the same shared well in every module. Semantic
    // highlights and user-specified chat backgrounds remain untouched.
    let bg = if span.background == theme.panel_bg.to_hex()
        || span.background == theme.bg.to_hex()
        || span.background == theme.row_bg.to_hex()
    {
        &visual.workspace.well
    } else {
        &span.background
    };
    if bg != &visual.workspace.well {
        surface.fill(r, bg, 0);
    }
    if text.trim().is_empty() {
        return;
    }
    surface.nodes.push(Primitive::Text {
        rect: r,
        text,
        color: span.foreground.clone(),
        size: visual.metrics.font,
        bold: span.bold,
        mono: true,
    });
    let modifiers = Modifier::from_bits_truncate(span.modifiers);
    if modifiers.contains(Modifier::UNDERLINED) && r.height > 2 {
        surface.fill(R::new(r.x, r.height - 2, r.width, 1), &span.foreground, 0);
    }
    if modifiers.contains(Modifier::CROSSED_OUT) {
        surface.fill(R::new(r.x, r.height / 2, r.width, 1), &span.foreground, 0);
    }
}

fn key_code(code: &str) -> Option<KeyCode> {
    if let Some(text) = code.strip_prefix("char:") {
        let mut chars = text.chars();
        let c = chars.next()?;
        return chars.next().is_none().then_some(KeyCode::Char(c));
    }
    if let Some(n) = code.strip_prefix("f:") {
        return n
            .parse::<u8>()
            .ok()
            .filter(|n| (1..=24).contains(n))
            .map(KeyCode::F);
    }
    Some(match code {
        "enter" => KeyCode::Enter,
        "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        _ => return None,
    })
}

fn mouse_kind(action: &str, button: u8) -> Option<MouseEventKind> {
    let button = match button {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        _ => return None,
    };
    Some(match action {
        "down" => MouseEventKind::Down(button),
        "up" => MouseEventKind::Up(button),
        "drag" => MouseEventKind::Drag(button),
        "move" => MouseEventKind::Moved,
        "scroll_up" => MouseEventKind::ScrollUp,
        "scroll_down" => MouseEventKind::ScrollDown,
        "scroll_left" => MouseEventKind::ScrollLeft,
        "scroll_right" => MouseEventKind::ScrollRight,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, discord::snowflake::ChannelId, ui::fake};
    use proptest::prelude::*;
    use starkit::graphics::Graphics;

    fn loaded() -> (Native, fake::Idle) {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/gateway/session.json");
        let fixture = fake::Session::read(&path).unwrap();
        let (core, idle) = fake::loaded(&fixture, 1_000);
        let mut app = App::new(
            core,
            Config::default(),
            "/nonexistent/config.toml".into(),
            None,
            Graphics::disabled(),
        );
        app.tz = jiff::tz::TimeZone::UTC;
        app.tick();
        app.open_channel(ChannelId(200000000000000011));
        app.tick();
        (Native::new(app), idle)
    }

    fn assert_bounded(scene: &Scene) {
        scene.viewport.validate().unwrap();
        assert!(scene.placements.len() <= 16);
        for component in &scene.components {
            let r = component.rect();
            assert!(u32::from(r.x) + u32::from(r.width) <= u32::from(scene.viewport.columns));
            assert!(u32::from(r.y) + u32::from(r.height) <= u32::from(scene.viewport.rows));
            if let Component::Surface { surface, .. } = component {
                surface.validate().unwrap();
            }
        }
        assert!(
            serde_json::to_vec(scene).unwrap().len()
                < starkit::terminal_graphics::protocol::MAX_MESSAGE
        );
    }

    #[test]
    fn every_pane_uses_shared_workspace_chrome_and_only_focus_gets_an_outline() {
        let (mut native, _idle) = loaded();
        native.app.focus_module(ModuleId::Conversation);
        let scene = native.scene(Viewport::default());
        assert_bounded(&scene);
        let panels: Vec<_> = scene
            .components
            .iter()
            .filter_map(|c| match c {
                Component::Surface { surface, .. } if surface.cell_size.is_none() => Some(surface),
                _ => None,
            })
            .collect();
        assert_eq!(panels.len(), 5);
        assert!(panels.iter().all(|p| p.ui_font));
        let borders: Vec<_> = panels
            .iter()
            .map(|p| {
                p.nodes
                    .iter()
                    .filter(|n| matches!(n, Primitive::Border { .. }))
                    .count()
            })
            .collect();
        assert_eq!(borders, [0, 0, 2, 0, 0]);
        assert!(panels
            .iter()
            .all(|p| p.background == native.visual().workspace.panel));
        // Exercise the actual KIT rasterizer, beyond serialized geometry.
        let image = starkit::terminal_graphics::renderer::render_reference(&scene).unwrap();
        assert_eq!(image.dimensions(), (1200, 800));
        let focus = pixels(
            native
                .app
                .layout
                .last
                .as_ref()
                .unwrap()
                .rect_of(ModuleId::Conversation),
            scene.viewport,
        );
        let accent = native.visual().palette.accent;
        assert!(image
            .enumerate_pixels()
            .any(|(x, y, p)| x >= u32::from(focus.x)
                && y >= u32::from(focus.y)
                && p.0[..3] == accent));
    }

    #[test]
    fn pointer_focus_uses_the_drawn_header_rectangles_and_preserves_the_accordion() {
        let (mut native, _idle) = loaded();
        let v = Viewport {
            width: 1199,
            height: 803,
            ..Viewport::default()
        }; // fractional cell edges
        native.scene(v);
        let focus = native.app.layout.focus();
        native.input(Input::Pointer {
            action: "down".into(),
            button: 0,
            x: u16::MAX,
            y: u16::MAX,
            pixel: Some([u32::MAX, u32::MAX]),
            modifiers: 0,
        });
        assert_eq!(native.app.layout.focus(), focus);
        let header = native
            .hits
            .iter()
            .find(|(_, a)| *a == Action::Pane(ModuleId::Members))
            .unwrap()
            .0;
        let input = || Input::Pointer {
            action: "down".into(),
            button: 0,
            x: 0,
            y: 0,
            pixel: Some([
                u32::from(header.x + header.width / 2),
                u32::from(header.y + header.height / 2),
            ]),
            modifiers: 0,
        };
        native.input(input());
        assert_eq!(native.app.layout.focus(), ModuleId::Members);
        assert_eq!(native.app.layout.expanded(), Some(ModuleId::Members));
        native.scene(v);
        let header = native
            .hits
            .iter()
            .find(|(_, a)| *a == Action::Pane(ModuleId::Members))
            .unwrap()
            .0;
        native.input(Input::Pointer {
            action: "down".into(),
            button: 0,
            x: 0,
            y: 0,
            pixel: Some([
                u32::from(header.x + header.width / 2),
                u32::from(header.y + header.height / 2),
            ]),
            modifiers: 0,
        });
        assert_eq!(native.app.layout.expanded(), None);
        assert_eq!(native.app.layout.focus(), ModuleId::Conversation);
    }

    #[test]
    fn presentation_switch_keeps_the_same_draft_channel_and_controller() {
        let (mut native, _idle) = loaded();
        native.app.focus_module(ModuleId::Compose);
        native.input(Input::Paste {
            text: "Keep this draft 👩‍💻".into(),
        });
        let channel = native.app.nav.channel;
        let before = native.scene(Viewport::default());
        native.input(Input::Key {
            code: "f:9".into(),
            modifiers: 0,
        });
        assert!(matches!(
            native.effects().as_slice(),
            [ServerMessage::TogglePresentation]
        ));
        assert!(native.effects().is_empty());
        native.presentation(true);
        let cells = native.scene(Viewport::default());
        assert!(!cells.spans.is_empty());
        assert!(cells.components.is_empty());
        assert_eq!(native.app.nav.channel, channel);
        assert!(cells
            .spans
            .iter()
            .any(|s| s.text.contains("Keep this draft")));
        native.presentation(false);
        let after = native.scene(Viewport::default());
        assert_bounded(&after);
        assert_eq!(before.components, after.components);
    }

    #[test]
    fn native_overlays_are_bounded_and_block_background_header_actions() {
        let (mut native, _idle) = loaded();
        native.scene(Viewport::default());
        let header = native
            .hits
            .iter()
            .find(|(_, a)| *a == Action::Pane(ModuleId::Members))
            .unwrap()
            .0;
        let focus = native.app.layout.focus();
        native.app.over.toggle_help();
        let scene = native.scene(Viewport::default());
        assert_bounded(&scene);
        assert!(scene
            .components
            .iter()
            .any(|c| matches!(c, Component::Menu { .. })));
        native.input(Input::Pointer {
            action: "down".into(),
            button: 0,
            x: 0,
            y: 0,
            pixel: Some([u32::from(header.x), u32::from(header.y)]),
            modifiers: 0,
        });
        assert_eq!(native.app.layout.focus(), focus);
        starkit::terminal_graphics::renderer::render_reference(&scene).unwrap();
    }

    #[test]
    fn cell_fallback_and_viewport_floor_recover_without_stale_hit_regions() {
        let (mut native, _idle) = loaded();
        for (columns, rows, width, height) in [
            (100, 40, 1199, 803),
            (60, 21, 480, 378),
            (59, 20, 472, 360),
            (100, 40, 1200, 800),
        ] {
            let scene = native.scene(Viewport {
                columns,
                rows,
                width,
                height,
                generation: 2,
            });
            assert_bounded(&scene);
            if columns < 60 {
                assert!(native.hits.is_empty());
            }
        }
        let mut capabilities = Capabilities::detected(&Graphics::disabled());
        capabilities.cell_presentation = Some(true);
        native.capabilities(capabilities);
        assert!(!native.scene(Viewport::default()).spans.is_empty());
        native.detached();
        assert!(!native.app.terminal_focused);
        native.input(Input::Pointer {
            action: "move".into(),
            button: 0,
            x: 0,
            y: 0,
            pixel: Some([0, 0]),
            modifiers: 0,
        });
        assert!(
            !native.app.terminal_focused,
            "hover must not resume read acknowledgements"
        );
        native.input(Input::Notice {
            message: "starcord:focus-gained".into(),
        });
        assert!(native.app.terminal_focused);
    }

    #[test]
    fn workspace_derivation_honors_explicit_chat_colors() {
        use starkit::theme::{Resolve, ThemeFile, BUILTINS};
        let text = BUILTINS
            .iter()
            .find(|b| b.id == "kanagawa")
            .unwrap()
            .toml
            .to_owned()
            + "\n[chat]\ntime_fg = '#c0a36e'\nmention_bg = '#223249'\n";
        let file: ThemeFile = toml::from_str(&text).unwrap();
        let theme = crate::ui::theme::Theme::resolve(&file);
        let visual = Visual::new(palette(&theme));
        let adapted = theme.workspace(&visual);
        assert_eq!(adapted.chat.time_fg, theme.chat.time_fg);
        assert_eq!(adapted.chat.mention_bg, theme.chat.mention_bg);
        assert!(adapted.chat.author_fg.contrast(adapted.panel_bg) >= 4.5);
    }

    proptest! {
        #[test]
        fn native_density_stays_in_kits_viewport_budget(width in 1u32..8193,height in 1u32..8193) {
            prop_assume!(u64::from(width)*u64::from(height)<=32_000_000);
            let v=native_viewport(Viewport {width,height,..Viewport::default()});
            prop_assert!(v.validate().is_ok());
        }
        #[test]
        fn named_character_keys_round_trip_without_accepting_trailing_input(c in any::<char>(), suffix in ".{1,32}") {
            prop_assert_eq!(key_code(&format!("char:{c}")),Some(KeyCode::Char(c)));
            prop_assert_eq!(key_code(&format!("char:{c}{suffix}")),None);
        }
        #[test]
        fn clipped_foreign_text_stays_within_a_native_surface(text in ".{0,256}", start in 0u16..20, width in 1u16..80) {
            let theme=crate::ui::theme::tests_support::theme("kanagawa");
            let visual=Visual::new(palette(&theme));
            let mut surface=Surface::new(width*8,18,visual.workspace.well.clone()).with_ui_text();
            let span=Span{x:0,y:0,text,foreground:theme.fg.to_hex(),background:theme.bg.to_hex(),bold:false,modifiers:0};
            append_span(&mut surface,&span,Rect::new(start,0,width,1),(8,18),&visual,&theme);
            prop_assert!(surface.validate().is_ok());
        }
    }
}
