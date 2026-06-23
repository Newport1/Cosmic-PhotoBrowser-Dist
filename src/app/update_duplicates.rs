//! update() handlers for Find Duplicates (perceptual dHash) and Find Exact Duplicates (SHA-256).

use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_find_duplicates(&mut self) -> Task<cosmic::Action<Message>> {
        if (self.dups.filter_active && !self.dups.exact) || self.dups.scan_in_progress {
            // Turn it off and clear state.
            self.dups.filter_active = false;
            self.dups.groups.clear();
            self.dups.members.clear();
            self.dups.scan_in_progress = false;
            self.dups.exact = false;
            self.scroll_offset_y = 0.0;
            self.rebuild_snapshot();
            return Task::none();
        }
        // Start a background scan over loupe-eligible entries (Image/Raw).
        let pairs: Vec<(usize, std::path::PathBuf)> = self
            .browser
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| crate::view::loupe::can_open_loupe(&e.kind))
            .map(|(idx, e)| (idx, e.path.clone()))
            .collect();
        if pairs.is_empty() {
            self.dups.filter_active = false;
            self.dups.groups.clear();
            self.dups.members.clear();
            self.dups.scan_in_progress = false;
            self.dups.exact = false;
            return Task::none();
        }
        self.dups.scan_in_progress = true;
        self.dups.exact = false;

        // The background task opens its own catalog connection.
        let db = crate::catalog::catalog_db_path();
        Task::perform(
            async move { crate::tasks::hash_entries(pairs, db) },
            |items| cosmic::Action::App(Message::DuplicatesScanned(items)),
        )
    }

    pub(super) fn handle_duplicates_scanned(
        &mut self,
        items: Vec<(usize, u64)>,
    ) -> Task<cosmic::Action<Message>> {
        self.dups.scan_in_progress = false;
        let (groups, members) = super::compute_duplicate_sets(&items, super::DUP_HAMMING_THRESHOLD);
        self.dups.groups = groups;
        self.dups.members = members;
        self.dups.filter_active = true;
        self.dups.exact = false;
        self.scroll_offset_y = 0.0;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_find_exact_duplicates(&mut self) -> Task<cosmic::Action<Message>> {
        // Toggle off only if an EXACT result is currently shown.
        if (self.dups.filter_active && self.dups.exact) || self.dups.scan_in_progress {
            self.dups.filter_active = false;
            self.dups.groups.clear();
            self.dups.members.clear();
            self.dups.scan_in_progress = false;
            self.dups.exact = false;
            self.scroll_offset_y = 0.0;
            self.rebuild_snapshot();
            return Task::none();
        }
        let pairs: Vec<(usize, std::path::PathBuf)> = self
            .browser
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| crate::view::loupe::can_open_loupe(&e.kind))
            .map(|(idx, e)| (idx, e.path.clone()))
            .collect();
        if pairs.is_empty() {
            self.dups.filter_active = false;
            self.dups.groups.clear();
            self.dups.members.clear();
            self.dups.scan_in_progress = false;
            self.dups.exact = false;
            return Task::none();
        }
        self.dups.scan_in_progress = true;
        self.dups.exact = true;

        // The background task opens its own catalog connection.
        let db = crate::catalog::catalog_db_path();
        Task::perform(
            async move { crate::tasks::hash_entries_sha256(pairs, db) },
            |items| cosmic::Action::App(Message::ExactDuplicatesScanned(items)),
        )
    }

    pub(super) fn handle_exact_duplicates_scanned(
        &mut self,
        items: Vec<(usize, String)>,
    ) -> Task<cosmic::Action<Message>> {
        self.dups.scan_in_progress = false;
        let groups = crate::dedupe::group_exact(&items);
        let members: std::collections::HashSet<usize> =
            groups.iter().flat_map(|g| g.iter().copied()).collect();
        self.dups.groups = groups;
        self.dups.members = members;
        self.dups.filter_active = true;
        self.dups.exact = true;
        self.scroll_offset_y = 0.0;
        self.rebuild_snapshot();
        Task::none()
    }
}
