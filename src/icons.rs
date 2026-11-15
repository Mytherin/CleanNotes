//! Toolbar and sidebar icons, drawn as shapes so they're crisp at any scale
//! (in the spirit of SF Symbols).

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Sidebar,
    List,
    Gallery,
    Trash,
    Compose,
    Format,
    Checklist,
    Search,
    Folder,
    Tag,
    PlusCircle,
    Pin,
    Link,
    Sync,
    Warning,
    Gear,
    ChevronRight,
    ChevronDown,
    /// A folder with a plus: New Folder.
    FolderPlus,
    /// "…": more actions.
    More,
}

fn line(p: &Painter, pts: &[Pos2], s: Stroke) {
    p.add(Shape::line(pts.to_vec(), s));
}

/// Draws `icon` centered in `r`, about `r.height()` tall.
pub fn paint(p: &Painter, icon: Icon, r: Rect, color: Color32) {
    let c = r.center();
    let h = r.height().min(r.width());
    let u = h / 16.0;
    let s = Stroke::new((1.3 * u).max(1.0), color);
    let at = |x: f32, y: f32| Pos2::new(c.x + x * u, c.y + y * u);
    match icon {
        Icon::Sidebar => {
            let b = Rect::from_center_size(c, Vec2::new(15.0 * u, 12.0 * u));
            p.rect_stroke(b, 2.5 * u, s);
            line(p, &[at(-2.5, -6.0), at(-2.5, 6.0)], s);
            for y in [-3.0, -0.5, 2.0] {
                line(p, &[at(-6.0, y), at(-4.2, y)], s);
            }
        }
        Icon::List => {
            for y in [-4.5, 0.0, 4.5] {
                p.circle_filled(at(-5.5, y), 1.0 * u, color);
                line(p, &[at(-3.0, y), at(6.5, y)], s);
            }
        }
        Icon::Gallery => {
            for (x, y) in [(-3.6, -3.6), (3.6, -3.6), (-3.6, 3.6), (3.6, 3.6)] {
                p.rect_stroke(Rect::from_center_size(at(x, y), Vec2::splat(5.6 * u)), 1.2 * u, s);
            }
        }
        Icon::Trash => {
            line(p, &[at(-6.5, -4.5), at(6.5, -4.5)], s);
            line(p, &[at(-2.5, -4.5), at(-2.0, -6.5), at(2.0, -6.5), at(2.5, -4.5)], s);
            line(p, &[at(-5.0, -4.5), at(-4.2, 6.5), at(4.2, 6.5), at(5.0, -4.5)], s);
            line(p, &[at(-1.6, -1.5), at(-1.4, 4.0)], s);
            line(p, &[at(1.6, -1.5), at(1.4, 4.0)], s);
        }
        Icon::Compose => {
            line(p, &[at(1.0, -6.0), at(-6.0, -6.0), at(-6.0, 6.5), at(6.5, 6.5), at(6.5, -0.5)], s);
            line(p, &[at(-1.5, 2.5), at(-1.0, 0.2), at(5.5, -6.3), at(7.3, -4.5), at(0.8, 2.0), at(-1.5, 2.5)], s);
        }
        Icon::Format => {
            let f = eframe::egui::FontId::new(13.0 * u, crate::theme::bold());
            p.text(at(-2.5, 0.8), eframe::egui::Align2::CENTER_CENTER, "A", f, color);
            let f = eframe::egui::FontId::new(10.0 * u, crate::theme::bold());
            p.text(at(4.5, 2.0), eframe::egui::Align2::CENTER_CENTER, "a", f, color);
        }
        Icon::Checklist => {
            for y in [-3.5, 3.5] {
                p.circle_stroke(at(-4.0, y), 2.4 * u, s);
                line(p, &[at(0.0, y), at(7.0, y)], s);
            }
            line(p, &[at(-5.2, -3.6), at(-4.2, -2.5), at(-2.6, -4.8)], s);
        }
        Icon::Search => {
            p.circle_stroke(at(-1.2, -1.2), 4.6 * u, s);
            line(p, &[at(2.2, 2.2), at(6.0, 6.0)], Stroke::new(s.width * 1.3, color));
        }
        Icon::Folder => {
            let pts = vec![
                at(-7.0, -4.5),
                at(-2.5, -4.5),
                at(-1.0, -3.0),
                at(7.0, -3.0),
                at(7.0, 5.5),
                at(-7.0, 5.5),
                at(-7.0, -4.5),
            ];
            line(p, &pts, s);
            line(p, &[at(-7.0, -1.0), at(7.0, -1.0)], s);
        }
        Icon::Tag => {
            let f = eframe::egui::FontId::new(14.0 * u, crate::theme::bold());
            p.text(c, eframe::egui::Align2::CENTER_CENTER, "#", f, color);
        }
        Icon::PlusCircle => {
            p.circle_stroke(c, 6.5 * u, s);
            line(p, &[at(-3.2, 0.0), at(3.2, 0.0)], s);
            line(p, &[at(0.0, -3.2), at(0.0, 3.2)], s);
        }
        Icon::Pin => {
            let pts = vec![at(-2.5, -6.0), at(2.5, -6.0), at(2.0, -1.5), at(4.0, 1.0), at(-4.0, 1.0), at(-2.0, -1.5)];
            p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
            line(p, &[at(0.0, 1.0), at(0.0, 6.5)], s);
        }
        Icon::Link => {
            p.rect_stroke(Rect::from_center_size(at(-2.2, 2.2), Vec2::new(8.0 * u, 4.5 * u)), 2.2 * u, s);
            p.rect_stroke(Rect::from_center_size(at(2.2, -2.2), Vec2::new(8.0 * u, 4.5 * u)), 2.2 * u, s);
        }
        Icon::Sync => {
            let arc = |a0: f32, a1: f32| -> Vec<Pos2> {
                (0..=12).map(|i| {
                    let a = a0 + (a1 - a0) * i as f32 / 12.0;
                    at(5.5 * a.cos(), 5.5 * a.sin())
                }).collect()
            };
            line(p, &arc(3.6, 6.0), s);
            line(p, &arc(0.5, 2.9), s);
            line(p, &[at(4.0, -5.5), at(5.4, -2.6), at(2.3, -2.6)], s);
            line(p, &[at(-4.0, 5.5), at(-5.4, 2.6), at(-2.3, 2.6)], s);
        }
        Icon::Warning => {
            let pts = vec![at(0.0, -6.5), at(7.0, 6.0), at(-7.0, 6.0), at(0.0, -6.5)];
            line(p, &pts, s);
            line(p, &[at(0.0, -2.0), at(0.0, 2.0)], s);
            p.circle_filled(at(0.0, 4.0), 0.9 * u, color);
        }
        Icon::Gear => {
            p.circle_stroke(c, 2.5 * u, s);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                line(p, &[at(4.5 * a.cos(), 4.5 * a.sin()), at(6.8 * a.cos(), 6.8 * a.sin())], Stroke::new(s.width * 1.6, color));
            }
            p.circle_stroke(c, 4.8 * u, s);
        }
        Icon::ChevronRight => line(p, &[at(-1.5, -4.0), at(2.5, 0.0), at(-1.5, 4.0)], s),
        Icon::ChevronDown => line(p, &[at(-4.0, -1.5), at(0.0, 2.5), at(4.0, -1.5)], s),
        Icon::FolderPlus => {
            let pts = vec![at(-7.0, -4.5), at(-2.5, -4.5), at(-1.0, -3.0), at(2.0, -3.0)];
            line(p, &pts, s);
            line(p, &[at(7.0, 1.5), at(7.0, 5.5), at(-7.0, 5.5), at(-7.0, -4.5)], s);
            line(p, &[at(-7.0, -1.0), at(1.5, -1.0)], s);
            p.circle_filled(at(5.0, -3.5), 3.8 * u, color);
            let cut = Stroke::new(s.width, if color.r() > 128 { Color32::BLACK } else { Color32::WHITE });
            line(p, &[at(3.0, -3.5), at(7.0, -3.5)], cut);
            line(p, &[at(5.0, -5.5), at(5.0, -1.5)], cut);
        }
        Icon::More => {
            for x in [-5.0, 0.0, 5.0] {
                p.circle_filled(at(x, 0.0), 1.3 * u, color);
            }
        }
    }
}
