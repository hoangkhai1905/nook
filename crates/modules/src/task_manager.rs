//! Task Manager: a 1x1 launcher. Click to open Windows Task Manager, or
//! bring it forward when it is already running.
//!
//! Task Manager always runs elevated, so a plain `CreateProcess` fails
//! (error 740); launching goes through the shell, which asks for consent.

use egui::{Sense, Ui};
use nook_platform::{media, shell};
use nook_ui::module::{Module, TileCtx, TileSize};
use nook_ui::theme::SIZE_ICON;
use nook_ui::{icons, paint};

#[derive(Default)]
pub struct TaskManager {
    /// Set after bringing it forward; the app then closes the panel.
    close: bool,
}

impl Module for TaskManager {
    fn id(&self) -> &'static str {
        "task_manager"
    }

    fn name(&self) -> &str {
        "Task Manager"
    }

    fn icon(&self) -> &'static str {
        icons::CHART_LINE
    }

    fn size_hint(&self) -> TileSize {
        TileSize::Small
    }

    fn take_close_request(&mut self) -> bool {
        std::mem::take(&mut self.close)
    }

    fn ui_tile(&mut self, ui: &mut Ui, ctx: &mut TileCtx<'_>) {
        let rect = ui.max_rect();
        let resp = ui
            .interact(
                rect.expand(8.0),
                ui.id().with("task-manager"),
                Sense::click(),
            )
            .on_hover_text("Open Task Manager");
        let color = if ctx.hovered {
            ctx.theme.text
        } else {
            ctx.theme.text_dim
        };
        paint::icon(
            ui.painter(),
            rect.center(),
            icons::CHART_LINE,
            SIZE_ICON + 2.0,
            color,
            false,
        );
        if !resp.clicked() {
            return;
        }
        // Its window belongs to Taskmgr.exe; no hints, any window will do.
        match media::focus_app("Taskmgr", &[]) {
            Ok(true) => self.close = true,
            _ => {
                if let Err(e) = shell::open("taskmgr.exe") {
                    tracing::warn!("task manager: {e}");
                }
            }
        }
    }
}
