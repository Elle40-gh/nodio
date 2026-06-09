#![deny(clippy::all)]
use std::sync::Arc;
use std::time::Duration;

use eframe::{egui, App, CreationContext, Frame, NativeOptions, Storage};
use egui::{
    pos2, Color32, FontData, FontDefinitions, FontFamily, RichText, Style, ViewportCommand, Widget,
};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};
use indexmap::IndexMap;
use log::{debug, warn};
use parking_lot::RwLock;
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

use nodio_api::create_nodio_context;
use nodio_core::{Context, DeviceInfo, ProcessInfo, Uuid};
use nodio_core::{Node, NodeKind};
use nodio_gui_nodes::{AttributeFlags, Context as NodeContext, LinkArgs, PinArgs};
use slider::VolumeSlider;

use crate::egui::{Direction, Pos2, Response, Ui};

mod slider;

/// Two node circles connected by a 1.5-cycle sine wave, rendered at 32×32 RGBA.
/// Shared by both the tray icon and the window icon.
fn icon_rgba() -> Vec<u8> {
    const W: u32 = 32;
    const H: u32 = 32;
    let mut rgba = vec![0u8; (W * H * 4) as usize];

    // Anti-aliased filled disk. Max-alpha blending lets overlapping strokes accumulate
    // without double-darkening.
    fn paint(rgba: &mut [u8], w: u32, h: u32, cx: f32, cy: f32, radius: f32) {
        let x0 = (cx - radius - 1.0).max(0.0) as u32;
        let x1 = ((cx + radius + 2.0).min(w as f32)) as u32;
        let y0 = (cy - radius - 1.0).max(0.0) as u32;
        let y1 = ((cy + radius + 2.0).min(h as f32)) as u32;
        for py in y0..y1 {
            for px in x0..x1 {
                let dx = px as f32 + 0.5 - cx;
                let dy = py as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let a = ((radius - dist + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
                if a == 0 {
                    continue;
                }
                let i = ((py * w + px) * 4) as usize;
                if a > rgba[i + 3] {
                    rgba[i] = 100;
                    rgba[i + 1] = 210;
                    rgba[i + 2] = 255;
                    rgba[i + 3] = a;
                }
            }
        }
    }

    paint(&mut rgba, W, H, 5.5, 16.0, 4.5); // left node
    paint(&mut rgba, W, H, 26.5, 16.0, 4.5); // right node
    for step in 0..=120_u32 {
        let t = step as f32 / 120.0;
        let wx = 9.5 + t * 13.0;
        let wy = 16.0 + 5.0 * (t * std::f32::consts::PI * 3.0).sin();
        paint(&mut rgba, W, H, wx, wy, 1.5);
    }

    rgba
}

fn make_tray_icon() -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(icon_rgba(), 32, 32).expect("valid icon")
}

fn make_app_icon() -> egui::viewport::IconData {
    egui::viewport::IconData {
        rgba: icon_rgba(),
        width: 32,
        height: 32,
    }
}

fn main() {
    pretty_env_logger::init();

    let start_hidden = std::env::args().any(|a| a == nodio_core::STARTUP_MINIMIZED_ARG);

    eframe::run_native(
        "Nodio",
        NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1200.0, 800.0])
                .with_visible(!start_hidden)
                .with_icon(make_app_icon()),
            ..Default::default()
        },
        Box::new(move |cc| Ok(setup_app(cc, start_hidden))),
    )
    .unwrap();
}

fn setup_app(setup_ctx: &CreationContext, start_hidden: bool) -> Box<dyn App + 'static> {
    let mut style = Style::default();
    style.visuals.override_text_color = Some(Color32::from_rgb(225, 225, 225));
    style.visuals.widgets.noninteractive.bg_fill = Color32::from_rgba_unmultiplied(50, 50, 50, 255);
    setup_ctx.egui_ctx.set_global_style(style);

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "custom".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../fonts/Lato-Regular.ttf"
        ))),
    );
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .unwrap()
        .insert(0, "custom".to_owned());
    fonts
        .families
        .get_mut(&FontFamily::Monospace)
        .unwrap()
        .push("custom".to_owned());
    setup_ctx.egui_ctx.set_fonts(fonts);

    let mut app = MyApp::new(start_hidden);

    if let Some(nodes_json) = setup_ctx
        .storage
        .and_then(|storage| storage.get_string("nodes"))
    {
        let mut ctx = app.ctx.write();
        for node in serde_json::from_str::<Vec<_>>(&nodes_json).unwrap_or_default() {
            ctx.add_node(node);
        }
    }

    if let Some(links_json) = setup_ctx
        .storage
        .and_then(|storage| storage.get_string("links"))
    {
        let mut ctx = app.ctx.write();
        for (id, start, end) in serde_json::from_str::<Vec<_>>(&links_json).unwrap_or_default() {
            if app.ui_links.insert(id, (start, end)).is_none() {
                ctx.connect_node(start, end).ok();
            }
        }
    }

    Box::new(app)
}

#[derive(Copy, Clone)]
enum ContextMenuKind {
    Node(Uuid),
    Editor,
}

struct MyApp {
    ctx: Arc<RwLock<dyn Context>>,
    node_ctx: NodeContext,
    /// Links between nodes (id, (start -> end))
    ui_links: IndexMap<Uuid, (Uuid, Uuid)>,
    context_menu_kind: Option<ContextMenuKind>,
    detached_link: Option<(Uuid, Uuid)>,
    should_save: bool,
    // Tray state — _tray must stay alive or the icon disappears
    _tray: TrayIcon,
    show_id: MenuId,
    quit_id: MenuId,
    startup_item: CheckMenuItem,
    startup_id: MenuId,
    visible: bool,
    /// Set before requesting a real close so close-interception lets it through
    should_quit: bool,
}

impl MyApp {
    fn new(start_hidden: bool) -> Self {
        let ctx = create_nodio_context();

        let show_item = MenuItem::new("Show UI", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        let startup_item =
            CheckMenuItem::new("Run on startup", true, ctx.read().run_at_startup(), None);
        let show_id = show_item.id().clone();
        let quit_id = quit_item.id().clone();
        let startup_id = startup_item.id().clone();

        let menu = Menu::new();
        menu.append(&show_item).expect("append show");
        menu.append(&PredefinedMenuItem::separator())
            .expect("append separator");
        menu.append(&startup_item).expect("append startup");
        menu.append(&PredefinedMenuItem::separator())
            .expect("append separator");
        menu.append(&quit_item).expect("append quit");

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Nodio")
            .with_icon(make_tray_icon())
            .build()
            .expect("tray icon");

        Self {
            ctx,
            node_ctx: NodeContext::default(),
            ui_links: IndexMap::new(),
            context_menu_kind: None,
            detached_link: None,
            should_save: false,
            _tray: tray,
            show_id,
            quit_id,
            startup_item,
            startup_id,
            visible: !start_hidden,
            should_quit: false,
        }
    }

    fn interact_and_draw(&mut self, ui_ctx: &egui::Context, ui: &mut Ui) {
        let node_count = self.ctx.read().nodes().len();

        let mut toasts = Toasts::new()
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::pos2(-10.0, -10.0))
            .direction(Direction::BottomUp);

        self.node_ctx.begin_frame(ui);

        for node_idx in 0..node_count {
            let Node {
                id: node_id,
                kind: node_kind,
                volume: mut node_volume,
                active: node_active,
                present: node_present,
                muted: node_muted,
                peak_values: node_peak_values,
                display_name: node_display_name,
                pos: node_pos,
                ..
            } = self.ctx.read().nodes().get(node_idx).cloned().unwrap();

            let pin_args = match node_kind {
                NodeKind::Application | NodeKind::InputDevice => PinArgs::default(),
                NodeKind::OutputDevice => PinArgs {
                    flags: Some(AttributeFlags::EnableLinkDetachWithDragClick as _),
                    ..Default::default()
                },
            };

            let header_contents = {
                let ctx = self.ctx.clone();
                move |ui: &mut Ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_enabled_ui(node_present, |ui| {
                            ui.horizontal(|ui| {
                                ui.add(egui::Label::new(&node_display_name).selectable(false));
                                let icon = if node_muted {
                                    "🔇"
                                } else if node_active {
                                    "🔉"
                                } else {
                                    "🔈"
                                };
                                if ui.small_button(icon).clicked() {
                                    ctx.write().set_muted(node_id, !node_muted);
                                }
                            })
                        });
                    });
                }
            };

            let attr_contents = {
                let ctx = self.ctx.clone();
                move |ui: &mut Ui| {
                    ui.vertical(|ui| {
                        ui.add_enabled_ui(node_present, |ui| {
                            ui.spacing_mut().slider_width = 130.0;

                            let r = VolumeSlider::new(&mut node_volume, node_peak_values).ui(ui);
                            if r.changed() {
                                ctx.write().set_volume(node_id, node_volume);
                            }
                            r
                        })
                        .inner
                    })
                    .inner
                }
            };

            let mut node = self
                .node_ctx
                .add_node(node_id)
                .with_origin(pos2(node_pos.0, node_pos.1))
                .with_header(header_contents);

            match node_kind {
                NodeKind::Application | NodeKind::InputDevice => {
                    node.with_output_attribute(node_id, pin_args, attr_contents);
                }
                NodeKind::OutputDevice => {
                    node.with_input_attribute(node_id, pin_args, attr_contents);
                }
            }

            node.show(ui);
        }

        for (&id, &(start, end)) in self.ui_links.iter() {
            self.node_ctx
                .add_link(id, start, end, LinkArgs::default(), ui);
        }

        let nodes_response = self.node_ctx.end_frame(ui);

        self.context_menu(nodes_response);

        if let Some(id) = self.node_ctx.detached_link() {
            debug!("link detached: {}", id);

            if let Some((from, to)) = self.ui_links.remove(&id) {
                self.ctx.write().disconnect_node(from, to);
                self.detached_link = Some((from, to));
            }
        }

        if let Some(id) = self.node_ctx.dropped_link() {
            debug!("link dropped: {}", id);

            self.should_save = true;
            self.detached_link = None;
        }

        if let Some((start, end, from_snap)) = self.node_ctx.created_link() {
            debug!("link created: {}, ({} to {})", start, end, from_snap);

            match self.ctx.write().connect_node(start, end) {
                Ok(()) => {
                    self.ui_links.retain(|_, link| *link != (start, end));
                    self.ui_links.insert(Uuid::new_v4(), (start, end));
                }
                Err(err) => {
                    warn!("Failed to connect nodes: {}", err);

                    toasts.add(Toast {
                        text: err.to_string().into(),
                        kind: ToastKind::Error,
                        options: ToastOptions::default().duration_in_seconds(10.0),
                        ..Default::default()
                    });

                    if let Some((from, to)) = self.detached_link.take() {
                        self.ui_links.insert(Uuid::new_v4(), (from, to));
                    }
                }
            }

            self.should_save = true;
        }

        if node_count == 0 {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Right-click anywhere to add nodes")
                        .heading()
                        .color(ui.visuals().widgets.inactive.text_color()),
                );
            });
        }

        if ui_ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
            self.remove_selected_nodes();
        }

        toasts.show(ui);
    }

    fn context_menu(&mut self, nodes_response: Response) {
        let context_menu_kind = self
            .context_menu_kind
            .take()
            .or_else(|| self.node_ctx.hovered_node().map(ContextMenuKind::Node))
            .unwrap_or(ContextMenuKind::Editor);

        nodes_response.context_menu(|ui| {
            self.context_menu_kind = Some(context_menu_kind);

            match context_menu_kind {
                ContextMenuKind::Node(node_id) => self.node_context_menu_items(ui, node_id),
                ContextMenuKind::Editor => self.editor_context_menu_items(ui),
            }
        });
    }

    fn node_context_menu_items(&mut self, ui: &mut Ui, node_id: Uuid) {
        if ui.button("Remove").clicked() {
            self.ctx.write().remove_node(node_id);
            self.ui_links
                .retain(|_, (start, end)| *start != node_id && *end != node_id);

            // Remove other nodes too, when multiple nodes selected
            self.remove_selected_nodes();

            ui.close();
        }
    }

    fn remove_selected_nodes(&mut self) {
        for &node_id in self.node_ctx.get_selected_nodes() {
            self.ctx.write().remove_node(node_id);
            self.ui_links
                .retain(|_, (start, end)| *start != node_id && *end != node_id);
        }
    }

    fn editor_context_menu_items(&mut self, ui: &mut Ui) {
        ui.set_min_width(160.0);
        let mut added_node = None;

        let menu_pos = ui
            .add_enabled_ui(false, |ui| ui.label("Add node"))
            .response
            .rect
            .min;

        ui.menu_button("Application", |ui| {
            ui.set_min_width(180.0);
            for process in self.ctx.read().application_processes() {
                Self::application_node_button(&mut added_node, menu_pos, ui, process);
            }
        });

        ui.menu_button("Input device", |ui| {
            ui.set_min_width(180.0);
            for device in self.ctx.read().input_devices() {
                Self::device_node_button(
                    &mut added_node,
                    menu_pos,
                    ui,
                    device,
                    NodeKind::InputDevice,
                );
            }
        });

        ui.menu_button("Output device", |ui| {
            ui.set_min_width(180.0);
            for device in self.ctx.read().output_devices() {
                Self::device_node_button(
                    &mut added_node,
                    menu_pos,
                    ui,
                    device,
                    NodeKind::OutputDevice,
                );
            }
        });

        if let Some(node) = added_node {
            self.ctx.write().add_node(node);
            self.should_save = true;
        }
    }

    fn application_node_button(
        added_node: &mut Option<Node>,
        menu_pos: Pos2,
        ui: &mut Ui,
        process: ProcessInfo,
    ) {
        if egui::Button::new(&process.display_name)
            .truncate()
            .ui(ui)
            .clicked()
        {
            added_node.replace(Node {
                kind: NodeKind::Application,
                display_name: process.display_name,
                filename: process.filename,
                pos: (menu_pos.x, menu_pos.y),
                process_id: Some(process.pid),
                ..Default::default()
            });
            ui.close();
        }
    }

    fn device_node_button(
        added_node: &mut Option<Node>,
        menu_pos: Pos2,
        ui: &mut Ui,
        device: DeviceInfo,
        node_kind: NodeKind,
    ) {
        if egui::Button::new(&device.name).truncate().ui(ui).clicked() {
            added_node.replace(Node {
                id: device.id,
                kind: node_kind,
                display_name: device.name,
                pos: (menu_pos.x, menu_pos.y),
                ..Default::default()
            });
            ui.close();
        }
    }
}

impl App for MyApp {
    // `logic` runs even when the window is hidden (if repaint was requested),
    // making it the right place for tray event polling and close interception.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        // Intercept window close — hide to tray instead of quitting
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.should_quit {
                // Tray "Quit" was clicked; let eframe proceed with closing
            } else {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.visible = false;
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            }
        }

        // Left-click on the tray icon → show the window
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                self.visible = true;
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
        }

        // Right-click tray context-menu events
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.show_id {
                self.visible = !self.visible;
                ctx.send_viewport_cmd(ViewportCommand::Visible(self.visible));
                if self.visible {
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                }
            } else if event.id == self.startup_id {
                let new_state = !self.ctx.read().run_at_startup();
                self.ctx.write().set_run_at_startup(new_state);
                self.startup_item.set_checked(new_state);
            } else if event.id == self.quit_id {
                self.should_quit = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }

        // When hidden, keep ticking so tray events are processed promptly
        if !self.visible {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        let ctx = ui.ctx().clone();
        self.interact_and_draw(&ctx, ui);
        // Continuous repaint keeps the audio-level visualization live
        ctx.request_repaint();
    }

    fn save(&mut self, storage: &mut dyn Storage) {
        debug!("Saving state");

        self.should_save = false;

        let mut nodes = self.ctx.read().nodes().to_vec();
        for node in nodes.iter_mut() {
            if let Some(pos) = self.node_ctx.node_pos(node.id) {
                node.pos = (pos.x, pos.y);
            }
        }

        let links: Vec<(Uuid, Uuid, Uuid)> = self
            .ui_links
            .iter()
            .map(|(id, (start, end))| (*id, *start, *end))
            .collect::<_>();

        storage.set_string("nodes", serde_json::to_string_pretty(&nodes).unwrap());
        storage.set_string("links", serde_json::to_string_pretty(&links).unwrap());
    }

    fn auto_save_interval(&self) -> Duration {
        if self.should_save {
            Duration::from_secs(0)
        } else {
            Duration::from_secs(30)
        }
    }
}
