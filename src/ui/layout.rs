//! Layout inspector: screenshot plus the UI hierarchy.

use egui::{Color32, Rect, RichText, Sense, Stroke, TextureHandle, Ui, Vec2};

use adbdeck::adb::layout::{extract_xml, UiTree};
use adbdeck::error::Error;
use adbdeck::jobs::Slot;

use super::screenshot::{decode, texture, Shot};
use super::widgets;
use super::{kit, theme, DeviceCtx, Env};

struct Snapshot {
    shot: Shot,
    tree: UiTree,
}

#[derive(Default)]
pub struct LayoutInspector {
    started: bool,
    load: Slot<Snapshot>,
    tex: Option<TextureHandle>,
    tree: Option<UiTree>,
    selected: Option<usize>,
    reveal: bool,
    error: Option<String>,
}

impl LayoutInspector {
    fn reload(&mut self, env: &Env, d: &DeviceCtx) {
        let dev = d.dev.clone();
        self.error = None;
        self.load.set(env.sp.spawn(async move {
            let (png, xml) = tokio::join!(dev.screenshot_png(), dev.dump_hierarchy());
            let (png, xml) = (png?, xml?);
            let xml = extract_xml(&xml).ok_or_else(|| Error::parse("uiautomator dump", "no hierarchy in the output"))?.to_string();
            Ok(Snapshot { shot: decode(png)?, tree: UiTree::parse(&xml)? })
        }));
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        if !self.started {
            self.started = true;
            self.reload(env, d);
        }
        if let Some(r) = self.load.poll() {
            match r {
                Ok(s) => {
                    self.tex = Some(texture(ui.ctx(), "layout-shot", &s.shot));
                    self.tree = Some(s.tree);
                    self.selected = None;
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        kit::card_margin(ui, 8, |ui| {
            ui.horizontal(|ui| {
                if kit::Btn::new("Capture again").icon(kit::ph::CAMERA).primary().enabled(!self.load.busy()).show(ui).clicked() {
                    self.reload(env, d);
                }
                if self.load.busy() {
                    ui.add(egui::Spinner::new().size(16.0));
                    ui.label(RichText::new("Capturing the screen and its layout…").color(theme::pal(ui).text2));
                }
            });
        });
        ui.add_space(10.0);
        if let Some(e) = self.error.clone() {
            if widgets::error_banner(ui, &e, true) {
                self.reload(env, d);
            }
            ui.add_space(8.0);
        }
        let (Some(tex), Some(tree)) = (self.tex.clone(), self.tree.take()) else {
            if !self.load.busy() && self.error.is_none() {
                kit::empty_state(ui, kit::ph::SELECTION, "Nothing captured", "Press Capture again to read the current screen.", |_| {});
            }
            return;
        };

        let pal = theme::pal(ui);
        let fr = egui::Frame::new()
            .fill(pal.card)
            .stroke(Stroke::new(1.0_f32, pal.stroke))
            .corner_radius(egui::CornerRadius::same(theme::RADIUS_CARD))
            .inner_margin(egui::Margin::same(14))
            .outer_margin(egui::Margin { left: 10, right: 0, top: 0, bottom: 0 });
        egui::SidePanel::right("layout-details")
            .resizable(true)
            .default_width(380.0)
            .frame(fr)
            .show_separator_line(false)
            .show_inside(ui, |ui| self.details(ui, env, &tree));
        let tree_ref = &tree;
        egui::CentralPanel::default().frame(egui::Frame::NONE).show_inside(ui, |ui| {
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                let size = tex.size_vec2();
                let scale = (ui.available_width() / size.x).min(ui.available_height().max(300.0) / size.y).min(1.0);
                let shown = size * scale;
                let (rect, resp) = ui.allocate_exact_size(shown, Sense::click());
                ui.painter().image(tex.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                let to_dev = |p: egui::Pos2| ((p.x - rect.min.x) / scale, (p.y - rect.min.y) / scale);
                let hover = resp.hover_pos().and_then(|p| {
                    let (x, y) = to_dev(p);
                    tree_ref.hit_test(x as i32, y as i32)
                });
                let node_rect = |id: usize| {
                    tree_ref.nodes[id].bounds.map(|b| {
                        Rect::from_min_max(
                            rect.min + Vec2::new(b.left as f32, b.top as f32) * scale,
                            rect.min + Vec2::new(b.right as f32, b.bottom as f32) * scale,
                        )
                    })
                };
                if let Some(r) = hover.and_then(node_rect) {
                    ui.painter().rect_stroke(r, 0.0, Stroke::new(1.5_f32, Color32::from_rgb(255, 170, 0)), egui::StrokeKind::Inside);
                }
                if let Some(r) = self.selected.and_then(node_rect) {
                    ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(40, 120, 255, 70));
                    ui.painter().rect_stroke(r, 0.0, Stroke::new(2.0_f32, Color32::from_rgb(40, 120, 255)), egui::StrokeKind::Inside);
                }
                if resp.clicked() {
                    if let Some(id) = hover {
                        self.selected = Some(id);
                        self.reveal = true;
                    }
                }
            });
        });
        self.tree = Some(tree);
    }

    fn details(&mut self, ui: &mut Ui, env: &mut Env, tree: &UiTree) {
        if let Some(id) = self.selected {
            let p = theme::pal(ui);
            ui.label(kit::semibold(tree.label(id)).size(14.0));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let mut chain = tree.ancestors(id);
                chain.reverse();
                for a in chain {
                    if ui.add(egui::Button::new(RichText::new(tree.label(a)).size(11.5).color(p.text2)).frame(false)).clicked() {
                        self.selected = Some(a);
                    }
                    ui.label(RichText::new(kit::ph::CARET_RIGHT).size(10.0).color(p.text3));
                }
            });
            let xpath = tree.xpath(id);
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(&xpath).monospace().size(11.5).color(p.text2)).selectable(true).truncate());
                if kit::icon_btn(ui, kit::ph::COPY, "Copy XPath").clicked() {
                    ui.ctx().copy_text(xpath.clone());
                    env.notes.info("XPath copied");
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().id_salt("layout-attrs").max_height(210.0).show(ui, |ui| {
                kit::kv_grid(ui, "node-attrs", |ui| {
                    for (k, v) in &tree.nodes[id].attrs {
                        if !v.is_empty() {
                            kit::kv(ui, k, v);
                        }
                    }
                });
            });
        } else {
            ui.label(RichText::new("Click an element in the screenshot, or choose one from the tree.").color(theme::pal(ui).text3));
        }
        ui.add_space(6.0);
        kit::hsep(ui);
        ui.add_space(6.0);
        ui.label(kit::semibold("Hierarchy").size(13.0));
        let selected = self.selected;
        let reveal = std::mem::take(&mut self.reveal);
        let open_ids: Vec<usize> = selected.map(|s| tree.ancestors(s)).unwrap_or_default();
        let mut clicked = None;
        egui::ScrollArea::both().id_salt("layout-tree").auto_shrink([false, false]).show(ui, |ui| {
            if !tree.nodes.is_empty() {
                node_ui(ui, tree, 0, selected, reveal, &open_ids, &mut clicked);
            }
        });
        if clicked.is_some() {
            self.selected = clicked;
        }
    }
}

fn node_ui(ui: &mut Ui, tree: &UiTree, id: usize, selected: Option<usize>, reveal: bool, open_ids: &[usize], clicked: &mut Option<usize>) {
    let node = &tree.nodes[id];
    let label = tree.label(id);
    if node.children.is_empty() {
        if ui.selectable_label(selected == Some(id), label).clicked() {
            *clicked = Some(id);
        }
        return;
    }
    let mut header = egui::CollapsingHeader::new(RichText::new(label).color(if selected == Some(id) {
        ui.visuals().selection.stroke.color
    } else {
        ui.visuals().text_color()
    }))
    .id_salt(("node", id))
    .default_open(id == 0);
    if reveal && open_ids.contains(&id) {
        header = header.open(Some(true));
    }
    let resp = header.show(ui, |ui| {
        for &c in &node.children {
            node_ui(ui, tree, c, selected, reveal, open_ids, clicked);
        }
    });
    if resp.header_response.clicked() {
        *clicked = Some(id);
    }
}
