//! update() handlers for export (File > Export selection).

use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_export_selection(&mut self) -> Task<cosmic::Action<Message>> {
        let sources: Vec<std::path::PathBuf> = self
            .cull_target_indices()
            .into_iter()
            .filter_map(|i| self.browser.entries.get(i))
            .filter(|e| crate::view::loupe::can_open_loupe(&e.kind))
            .map(|e| e.path.clone())
            .collect();
        let dest = self
            .config
            .export_dir
            .clone()
            .or_else(crate::config::default_export_dir);
        match (sources.is_empty(), dest) {
            (true, _) => {
                self.selection.batch_status = Some("Export: nothing selected".to_owned());
            }
            (false, None) => {
                self.selection.batch_status =
                    Some("Export failed: no destination folder".to_owned());
            }
            (false, Some(dest)) => {
                self.selection.batch_status = Some(format!("Exporting {}…", sources.len()));
                return crate::tasks::export_task(sources, dest);
            }
        }
        Task::none()
    }

    pub(super) fn handle_export_done(
        &mut self,
        res: crate::export::ExportResult,
        dest: std::path::PathBuf,
    ) -> Task<cosmic::Action<Message>> {
        self.selection.batch_status = Some(format!(
            "Exported {} ({} skipped, {} failed) → {}",
            res.copied,
            res.skipped,
            res.failed,
            dest.display()
        ));
        Task::none()
    }
}
