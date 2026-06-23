use std::path::Path;

use rusqlite::OptionalExtension;

#[allow(dead_code)]
pub struct Catalog {
    conn: rusqlite::Connection,
}

pub const SCHEMA_VERSION: i64 = 4;

/// Bump when EXIF/metadata extraction logic changes, to invalidate cached files.exif_captured_unix/camera produced by older code.
const META_VERSION: i64 = 1;

#[allow(dead_code)]
impl Catalog {
    /// Open/create the catalog DB at `db_path` (creates parent dirs). Runs migration.
    pub fn open(db_path: &Path) -> rusqlite::Result<Catalog> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                    Some(format!("failed to create parent dirs: {}", e)),
                )
            })?;
        }
        let conn = rusqlite::Connection::open(db_path)?;
        Self::init_conn(&conn)?;
        Ok(Catalog { conn })
    }

    /// In-memory catalog (for tests).
    pub fn open_in_memory() -> rusqlite::Result<Catalog> {
        let conn = rusqlite::Connection::open_in_memory()?;
        Self::init_conn(&conn)?;
        Ok(Catalog { conn })
    }

    fn init_conn(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        conn.pragma_update(None, "foreign_keys", "ON")?;

        // Performance pragmas: WAL + synchronous=NORMAL (set on every open)
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "busy_timeout", 5000i64)?;

        let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if user_version == 0 {
            // Fresh DB: create the full current schema (v4) in one shot.
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS files (
                  id INTEGER PRIMARY KEY,
                  path TEXT UNIQUE NOT NULL,
                  size_bytes INTEGER NOT NULL,
                  mtime_unix INTEGER NOT NULL,
                  exif_captured_unix INTEGER,
                  camera TEXT,
                  imported_at_unix INTEGER NOT NULL,
                  meta_version INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS file_hashes (
                  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                  sha256 TEXT,
                  dhash INTEGER
                );
                CREATE TABLE IF NOT EXISTS tags (
                  id INTEGER PRIMARY KEY,
                  name TEXT NOT NULL UNIQUE COLLATE NOCASE
                );
                CREATE TABLE IF NOT EXISTS file_tags (
                  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                  tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
                  approved INTEGER NOT NULL DEFAULT 0,
                  source TEXT,
                  PRIMARY KEY (file_id, tag_id)
                );
                CREATE INDEX IF NOT EXISTS idx_files_path ON files(path);
                CREATE INDEX IF NOT EXISTS idx_files_exif_captured ON files(exif_captured_unix);
                CREATE INDEX IF NOT EXISTS idx_files_camera ON files(camera);
                CREATE INDEX IF NOT EXISTS idx_file_tags_tag ON file_tags(tag_id);
                CREATE INDEX IF NOT EXISTS idx_file_tags_approved ON file_tags(approved);
                "#,
            )?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else {
            // Stepwise upgrade: apply only the steps the DB has not seen yet.
            if user_version < 2 {
                // v1 -> v2: add camera column + its indexes.
                conn.execute_batch(
                    r#"
                    ALTER TABLE files ADD COLUMN camera TEXT;
                    CREATE INDEX IF NOT EXISTS idx_files_exif_captured ON files(exif_captured_unix);
                    CREATE INDEX IF NOT EXISTS idx_files_camera ON files(camera);
                    "#,
                )?;
            }
            if user_version < 3 {
                // v2 -> v3: tags + file_tags tables and their indexes.
                conn.execute_batch(
                    r#"
                    CREATE TABLE IF NOT EXISTS tags (
                      id INTEGER PRIMARY KEY,
                      name TEXT NOT NULL UNIQUE COLLATE NOCASE
                    );
                    CREATE TABLE IF NOT EXISTS file_tags (
                      file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                      tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
                      approved INTEGER NOT NULL DEFAULT 0,
                      source TEXT,
                      PRIMARY KEY (file_id, tag_id)
                    );
                    CREATE INDEX IF NOT EXISTS idx_file_tags_tag ON file_tags(tag_id);
                    CREATE INDEX IF NOT EXISTS idx_file_tags_approved ON file_tags(approved);
                    "#,
                )?;
            }
            if user_version < 4 {
                // v3 -> v4: add meta_version (default 0 = "pre-versioning, treat as stale").
                conn.execute_batch(
                    "ALTER TABLE files ADD COLUMN meta_version INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(())
    }

    /// Insert or update a file row (keyed by unique `path`); returns its `files.id`.
    /// On conflict(path): update size/mtime/exif/camera/imported. Idempotent on path.
    pub fn upsert_file(
        &self,
        path: &str,
        size_bytes: i64,
        mtime_unix: i64,
        exif_captured_unix: Option<i64>,
        camera: Option<&str>,
        imported_at_unix: i64,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO files (path, size_bytes, mtime_unix, exif_captured_unix, camera, imported_at_unix, meta_version)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(path) DO UPDATE SET
              size_bytes = excluded.size_bytes,
              mtime_unix = excluded.mtime_unix,
              exif_captured_unix = excluded.exif_captured_unix,
              camera = excluded.camera,
              imported_at_unix = excluded.imported_at_unix,
              meta_version = excluded.meta_version
            "#,
            rusqlite::params![
                path,
                size_bytes,
                mtime_unix,
                exif_captured_unix,
                camera,
                imported_at_unix,
                META_VERSION
            ],
        )?;

        // Return the id for this path
        let id: i64 = self.conn.query_row(
            "SELECT id FROM files WHERE path = ?1",
            rusqlite::params![path],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// (exif_captured_unix, camera) for a path, if the file row exists.
    pub fn get_file_meta(
        &self,
        path: &str,
    ) -> rusqlite::Result<Option<(Option<i64>, Option<String>)>> {
        self.conn
            .query_row(
                "SELECT exif_captured_unix, camera FROM files WHERE path = ?1",
                rusqlite::params![path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// (exif_captured_unix, camera) for `path` ONLY IF a files row exists AND its mtime_unix == `mtime`.
    /// Returns None if no row or mtime differs (so the caller knows to (re)index). Note: a fresh row with no
    /// EXIF legitimately returns Some((None, None)).
    pub fn get_file_meta_fresh(
        &self,
        path: &str,
        mtime: i64,
    ) -> rusqlite::Result<Option<(Option<i64>, Option<String>)>> {
        self.conn
            .query_row(
                "SELECT exif_captured_unix, camera FROM files WHERE path = ?1 AND mtime_unix = ?2 AND meta_version = ?3",
                rusqlite::params![path, mtime, META_VERSION],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// Store the dHash for `path` (file row must exist; upserts the file_hashes row).
    /// u64 is stored as i64 via bit-cast (`dhash as i64`).
    pub fn set_dhash(&self, path: &str, dhash: u64) -> rusqlite::Result<()> {
        let file_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                rusqlite::params![path],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(fid) = file_id {
            self.conn.execute(
                r#"
                INSERT INTO file_hashes (file_id, dhash)
                VALUES (?1, ?2)
                ON CONFLICT(file_id) DO UPDATE SET dhash = excluded.dhash
                "#,
                rusqlite::params![fid, dhash as i64],
            )?;
            Ok(())
        } else {
            // No file row for path: no-op (simplest, documented behavior)
            Ok(())
        }
    }

    /// Return the cached dHash for `path` ONLY IF the stored files.mtime_unix == `expect_mtime_unix`
    /// (mtime-invalidation) and a dhash is present; else Ok(None). Read back i64 -> u64 bit-cast.
    pub fn get_dhash(&self, path: &str, expect_mtime_unix: i64) -> rusqlite::Result<Option<u64>> {
        let result: Option<(i64, Option<i64>)> = self
            .conn
            .query_row(
                r#"
            SELECT f.mtime_unix, h.dhash
            FROM files f
            LEFT JOIN file_hashes h ON h.file_id = f.id
            WHERE f.path = ?1
            "#,
                rusqlite::params![path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match result {
            Some((mtime, Some(d))) if mtime == expect_mtime_unix => Ok(Some(d as u64)),
            _ => Ok(None),
        }
    }

    /// Store/replace the sha256 hex string for `path` (file row must exist).
    pub fn set_sha256(&self, path: &str, sha256: &str) -> rusqlite::Result<()> {
        let file_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                rusqlite::params![path],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(fid) = file_id {
            self.conn.execute(
                r#"
                INSERT INTO file_hashes (file_id, sha256)
                VALUES (?1, ?2)
                ON CONFLICT(file_id) DO UPDATE SET sha256 = excluded.sha256
                "#,
                rusqlite::params![fid, sha256],
            )?;
            Ok(())
        } else {
            Ok(())
        }
    }

    /// Return cached sha256 only if files.mtime_unix == expect_mtime_unix; else Ok(None).
    pub fn get_sha256(
        &self,
        path: &str,
        expect_mtime_unix: i64,
    ) -> rusqlite::Result<Option<String>> {
        let result: Option<(i64, Option<String>)> = self
            .conn
            .query_row(
                r#"
            SELECT f.mtime_unix, h.sha256
            FROM files f
            LEFT JOIN file_hashes h ON h.file_id = f.id
            WHERE f.path = ?1
            "#,
                rusqlite::params![path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match result {
            Some((mtime, Some(s))) if mtime == expect_mtime_unix => Ok(Some(s)),
            _ => Ok(None),
        }
    }

    /// Get-or-create a tag row by name (case-insensitive, stored trimmed); returns its id.
    /// Returns Ok(None) if `name` is empty/whitespace.
    fn tag_id_for(&self, name: &str) -> rusqlite::Result<Option<i64>> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO tags (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
            rusqlite::params![trimmed],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM tags WHERE name = ?1",
            rusqlite::params![trimmed],
            |row| row.get(0),
        )?;
        Ok(Some(id))
    }

    /// Attach `tag` to the file at `path` with the given approval + optional source.
    /// No-op if the files row doesn't exist or the tag name is empty. On conflict
    /// (file already has the tag) updates approved/source.
    pub fn add_tag(
        &self,
        path: &str,
        tag: &str,
        approved: bool,
        source: Option<&str>,
    ) -> rusqlite::Result<()> {
        let file_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                rusqlite::params![path],
                |row| row.get(0),
            )
            .optional()?;
        let (Some(fid), Some(tid)) = (file_id, self.tag_id_for(tag)?) else {
            return Ok(());
        };
        self.conn.execute(
            r#"
            INSERT INTO file_tags (file_id, tag_id, approved, source)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(file_id, tag_id) DO UPDATE SET
              approved = excluded.approved,
              source = excluded.source
            "#,
            rusqlite::params![fid, tid, approved as i64, source],
        )?;
        Ok(())
    }

    /// (tag name, approved) for the file at `path`, sorted by name. Empty if none / unknown path.
    pub fn get_tags(&self, path: &str) -> rusqlite::Result<Vec<(String, bool)>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT t.name, ft.approved
            FROM file_tags ft
            JOIN tags t ON t.id = ft.tag_id
            JOIN files f ON f.id = ft.file_id
            WHERE f.path = ?1
            ORDER BY t.name COLLATE NOCASE
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![path], |row| {
            let name: String = row.get(0)?;
            let approved: i64 = row.get(1)?;
            Ok((name, approved != 0))
        })?;
        rows.collect()
    }

    /// Set the approved flag for an existing (path, tag). No-op if the pairing doesn't exist.
    pub fn set_tag_approved(&self, path: &str, tag: &str, approved: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            r#"
            UPDATE file_tags
            SET approved = ?3
            WHERE file_id = (SELECT id FROM files WHERE path = ?1)
              AND tag_id  = (SELECT id FROM tags  WHERE name = ?2)
            "#,
            rusqlite::params![path, tag.trim(), approved as i64],
        )?;
        Ok(())
    }

    /// Remove `tag` from the file at `path`. No-op if not present.
    pub fn remove_tag(&self, path: &str, tag: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            r#"
            DELETE FROM file_tags
            WHERE file_id = (SELECT id FROM files WHERE path = ?1)
              AND tag_id  = (SELECT id FROM tags  WHERE name = ?2)
            "#,
            rusqlite::params![path, tag.trim()],
        )?;
        Ok(())
    }

    /// Paths that carry `tag` (any approval), sorted. For a future tag-filter UI.
    pub fn files_with_tag(&self, tag: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT f.path
            FROM files f
            JOIN file_tags ft ON ft.file_id = f.id
            JOIN tags t ON t.id = ft.tag_id
            WHERE t.name = ?1 COLLATE NOCASE
            ORDER BY f.path
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![tag.trim()], |row| row.get::<_, String>(0))?;
        rows.collect()
    }
}

/// Filesystem path of the catalog DB in PhotoBrowser's cache dir, or None if it can't be resolved.
pub fn catalog_db_path() -> Option<std::path::PathBuf> {
    crate::config::Config::project_dirs().map(|d| d.cache_dir().join("catalog.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_creates_schema_and_sets_user_version() {
        let cat = Catalog::open_in_memory().unwrap();

        // Querying tables should succeed
        let _ = cat.conn.execute("SELECT COUNT(*) FROM files", []);
        let _ = cat.conn.execute("SELECT COUNT(*) FROM file_hashes", []);

        let ver: i64 = cat
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(ver, 4);
    }

    #[test]
    fn upsert_file_returns_id_and_updates_in_place() {
        let cat = Catalog::open_in_memory().unwrap();

        let id1 = cat
            .upsert_file("/a/b.jpg", 1234, 1000, Some(900), None, 2000)
            .unwrap();
        assert!(id1 > 0);

        // Same path -> stable id, row count stays 1
        let id2 = cat
            .upsert_file("/a/b.jpg", 1234, 1000, Some(900), None, 2000)
            .unwrap();
        assert_eq!(id1, id2);

        let count: i64 = cat
            .conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path = ?1",
                rusqlite::params!["/a/b.jpg"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Update fields
        let id3 = cat
            .upsert_file("/a/b.jpg", 9999, 1111, None, None, 2222)
            .unwrap();
        assert_eq!(id1, id3);

        let (size, mtime, exif, imported): (i64, i64, Option<i64>, i64) = cat.conn.query_row(
            "SELECT size_bytes, mtime_unix, exif_captured_unix, imported_at_unix FROM files WHERE id = ?1",
            rusqlite::params![id1],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).unwrap();
        assert_eq!(size, 9999);
        assert_eq!(mtime, 1111);
        assert_eq!(exif, None);
        assert_eq!(imported, 2222);
    }

    #[test]
    fn set_get_dhash_roundtrips_including_high_bit() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/img.jpg", 10, 42, None, None, 1).unwrap();

        let high: u64 = 0xFFFF_0000_FFFF_0001;
        cat.set_dhash("/img.jpg", high).unwrap();
        let back = cat.get_dhash("/img.jpg", 42).unwrap();
        assert_eq!(back, Some(high));

        // Zero also works
        cat.set_dhash("/img.jpg", 0u64).unwrap();
        let back0 = cat.get_dhash("/img.jpg", 42).unwrap();
        assert_eq!(back0, Some(0u64));
    }

    #[test]
    fn get_dhash_mtime_invalidation() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/img.jpg", 10, 100, None, None, 1).unwrap();
        cat.set_dhash("/img.jpg", 0xABCD_EF01_2345_6789u64).unwrap();

        // Matching mtime -> Some
        let hit = cat.get_dhash("/img.jpg", 100).unwrap();
        assert!(hit.is_some());

        // Non-matching mtime -> None
        let miss = cat.get_dhash("/img.jpg", 101).unwrap();
        assert!(miss.is_none());
    }

    #[test]
    fn get_dhash_and_sha256_unknown_path_returns_none() {
        let cat = Catalog::open_in_memory().unwrap();
        assert_eq!(cat.get_dhash("/nope", 0).unwrap(), None);
        assert_eq!(cat.get_sha256("/nope", 0).unwrap(), None);
    }

    #[test]
    fn set_sha256_roundtrips_and_dhash_does_not_clobber_sha256() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/mix.jpg", 1, 10, None, None, 1).unwrap();

        cat.set_sha256("/mix.jpg", "deadbeefcafebabe").unwrap();
        cat.set_dhash("/mix.jpg", 0x1234_5678_9ABC_DEF0u64).unwrap();

        let s = cat.get_sha256("/mix.jpg", 10).unwrap();
        assert_eq!(s, Some("deadbeefcafebabe".to_string()));

        let d = cat.get_dhash("/mix.jpg", 10).unwrap();
        assert_eq!(d, Some(0x1234_5678_9ABC_DEF0u64));

        // Setting sha again does not affect dhash
        cat.set_sha256("/mix.jpg", "feedface").unwrap();
        let d2 = cat.get_dhash("/mix.jpg", 10).unwrap();
        assert_eq!(d2, Some(0x1234_5678_9ABC_DEF0u64));
        let s2 = cat.get_sha256("/mix.jpg", 10).unwrap();
        assert_eq!(s2, Some("feedface".to_string()));
    }

    #[test]
    fn persistence_across_open_close() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("cat.db");

        {
            let cat = Catalog::open(&db_path).unwrap();
            cat.upsert_file("rel/path.jpg", 777, 555, Some(444), None, 333)
                .unwrap();
            cat.set_dhash("rel/path.jpg", 0xDEAD_BEEF_FACE_CAFEu64)
                .unwrap();
            // Catalog drops here
        }

        {
            let cat2 = Catalog::open(&db_path).unwrap();
            let got = cat2.get_dhash("rel/path.jpg", 555).unwrap();
            assert_eq!(got, Some(0xDEAD_BEEF_FACE_CAFEu64));
        }
    }

    #[test]
    fn open_in_memory_reports_v2_and_camera_roundtrips() {
        let cat = Catalog::open_in_memory().unwrap();

        let ver: i64 = cat
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(ver, 4);

        let cap = 1_700_000_000i64;
        let cam = "Canon EOS R5";
        let id = cat
            .upsert_file("/photo.jpg", 12345, 1000, Some(cap), Some(cam), 2000)
            .unwrap();
        assert!(id > 0);

        let meta = cat.get_file_meta("/photo.jpg").unwrap();
        assert_eq!(meta, Some((Some(cap), Some(cam.to_string()))));
    }

    #[test]
    fn get_file_meta_unknown_and_none_values() {
        let cat = Catalog::open_in_memory().unwrap();

        assert_eq!(cat.get_file_meta("/nope").unwrap(), None);

        cat.upsert_file("/plain.jpg", 100, 10, None, None, 1)
            .unwrap();
        let meta = cat.get_file_meta("/plain.jpg").unwrap();
        assert_eq!(meta, Some((None, None)));
    }

    #[test]
    fn get_file_meta_fresh_matching_mtime_returns_some() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/a.jpg", 123, 1000, Some(900), Some("Canon"), 10)
            .unwrap();

        let hit = cat.get_file_meta_fresh("/a.jpg", 1000).unwrap();
        assert_eq!(hit, Some((Some(900), Some("Canon".to_string()))));
    }

    #[test]
    fn get_file_meta_fresh_nonmatching_mtime_returns_none() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/a.jpg", 123, 1000, Some(900), Some("Canon"), 10)
            .unwrap();

        let miss = cat.get_file_meta_fresh("/a.jpg", 1001).unwrap();
        assert_eq!(miss, None);
    }

    #[test]
    fn get_file_meta_fresh_unknown_path_returns_none() {
        let cat = Catalog::open_in_memory().unwrap();
        assert_eq!(cat.get_file_meta_fresh("/nope", 42).unwrap(), None);
    }

    #[test]
    fn get_file_meta_fresh_no_exif_returns_some_none_none() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/plain.jpg", 100, 10, None, None, 1)
            .unwrap();

        let meta = cat.get_file_meta_fresh("/plain.jpg", 10).unwrap();
        assert_eq!(meta, Some((None, None)));
    }

    #[test]
    fn get_file_meta_fresh_treats_old_meta_version_as_stale() {
        let cat = Catalog::open_in_memory().unwrap();
        // Manually insert a row with meta_version 0 (pre-versioning / stale)
        cat.conn
            .execute(
                "INSERT INTO files (path,size_bytes,mtime_unix,exif_captured_unix,camera,imported_at_unix,meta_version) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params!["/x.jpg", 1i64, 100i64, None::<i64>, "Canon", 1i64, 0i64],
            )
            .unwrap();

        // get_file_meta_fresh should treat meta_version 0 as stale -> None
        assert_eq!(cat.get_file_meta_fresh("/x.jpg", 100).unwrap(), None);

        // After upsert (which writes META_VERSION=1), it should be fresh
        cat.upsert_file("/x.jpg", 1, 100, Some(900), Some("Canon"), 1)
            .unwrap();
        assert_eq!(
            cat.get_file_meta_fresh("/x.jpg", 100).unwrap(),
            Some((Some(900), Some("Canon".to_string())))
        );
    }

    #[test]
    fn migration_v1_to_v2_adds_camera_and_indexes() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("v1cat.db");

        // Manually create a v1 schema
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE files (
                  id INTEGER PRIMARY KEY,
                  path TEXT UNIQUE NOT NULL,
                  size_bytes INTEGER NOT NULL,
                  mtime_unix INTEGER NOT NULL,
                  exif_captured_unix INTEGER,
                  imported_at_unix INTEGER NOT NULL
                );
                CREATE TABLE file_hashes (
                  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                  sha256 TEXT,
                  dhash INTEGER
                );
                CREATE INDEX idx_files_path ON files(path);
                "#,
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO files (path, size_bytes, mtime_unix, exif_captured_unix, imported_at_unix) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["/old.jpg", 999, 123, Some(456i64), 789i64],
            ).unwrap();
            conn.execute(
                "INSERT INTO file_hashes (file_id, dhash) SELECT id, 42 FROM files WHERE path = ?1",
                rusqlite::params!["/old.jpg"],
            )
            .unwrap();
        }

        // Now open via Catalog — must upgrade to v2
        let cat = Catalog::open(&db_path).unwrap();

        let ver: i64 = cat
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(ver, 4);

        // camera column exists (querying it succeeds) and old row's camera is NULL
        let meta = cat.get_file_meta("/old.jpg").unwrap();
        assert!(meta.is_some());
        let (cap, cam) = meta.unwrap();
        assert_eq!(cap, Some(456));
        assert_eq!(cam, None);

        // Old row data and hash intact
        let (size, mtime): (i64, i64) = cat
            .conn
            .query_row(
                "SELECT size_bytes, mtime_unix FROM files WHERE path = ?1",
                rusqlite::params!["/old.jpg"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(size, 999);
        assert_eq!(mtime, 123);

        let d = cat.get_dhash("/old.jpg", 123).unwrap();
        assert_eq!(d, Some(42u64));
    }

    #[test]
    fn open_sets_wal_journal_mode() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("walcat.db");

        let _cat = Catalog::open(&db_path).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let jm: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        // WAL mode reports "wal" (case-insensitive in some builds; normalize)
        assert_eq!(jm.to_lowercase(), "wal");
    }

    #[test]
    fn tags_v3_schema_present_and_version() {
        let cat = Catalog::open_in_memory().unwrap();

        let ver: i64 = cat
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(ver, 4);

        // Tables exist (queries succeed)
        let _ = cat.conn.execute("SELECT COUNT(*) FROM tags", []);
        let _ = cat.conn.execute("SELECT COUNT(*) FROM file_tags", []);
    }

    #[test]
    fn add_get_tags_roundtrip_case_insensitive_and_approved() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/photo.jpg", 100, 10, None, None, 1)
            .unwrap();

        cat.add_tag("/photo.jpg", "Sunset", false, Some("ai"))
            .unwrap();
        cat.add_tag("/photo.jpg", "beach", true, Some("user"))
            .unwrap();

        let tags = cat.get_tags("/photo.jpg").unwrap();
        assert_eq!(
            tags,
            vec![("beach".to_string(), true), ("Sunset".to_string(), false)]
        );

        // Re-adding same name (different case) with approved=true updates in place
        cat.add_tag("/photo.jpg", "SUNSET", true, Some("user"))
            .unwrap();
        let tags2 = cat.get_tags("/photo.jpg").unwrap();
        assert_eq!(tags2.len(), 2);
        assert_eq!(
            tags2,
            vec![("beach".to_string(), true), ("Sunset".to_string(), true)]
        );
    }

    #[test]
    fn set_tag_approved_and_remove_tag() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/img.jpg", 100, 10, None, None, 1).unwrap();
        cat.add_tag("/img.jpg", "foo", false, None).unwrap();

        let t1 = cat.get_tags("/img.jpg").unwrap();
        assert_eq!(t1, vec![("foo".to_string(), false)]);

        cat.set_tag_approved("/img.jpg", "foo", true).unwrap();
        let t2 = cat.get_tags("/img.jpg").unwrap();
        assert_eq!(t2, vec![("foo".to_string(), true)]);

        cat.remove_tag("/img.jpg", "foo").unwrap();
        let t3 = cat.get_tags("/img.jpg").unwrap();
        assert!(t3.is_empty());
    }

    #[test]
    fn add_tag_unknown_path_is_noop() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.add_tag("/nope.jpg", "bar", true, None).unwrap();

        // No tags table rows created for it
        let count: i64 = cat
            .conn
            .query_row("SELECT COUNT(*) FROM file_tags", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn add_tag_empty_name_is_noop() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/img.jpg", 100, 10, None, None, 1).unwrap();
        cat.add_tag("/img.jpg", "   ", true, None).unwrap();
        cat.add_tag("/img.jpg", "", false, None).unwrap();

        let tags = cat.get_tags("/img.jpg").unwrap();
        assert!(tags.is_empty());

        let tcount: i64 = cat
            .conn
            .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tcount, 0);
    }

    #[test]
    fn files_with_tag_returns_matching_paths() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.upsert_file("/a.jpg", 1, 10, None, None, 1).unwrap();
        cat.upsert_file("/b.jpg", 1, 10, None, None, 1).unwrap();
        cat.upsert_file("/c.jpg", 1, 10, None, None, 1).unwrap();

        cat.add_tag("/a.jpg", "sun", true, None).unwrap();
        cat.add_tag("/b.jpg", "Sun", false, None).unwrap();
        cat.add_tag("/c.jpg", "other", true, None).unwrap();

        let paths = cat.files_with_tag("sun").unwrap();
        assert_eq!(paths, vec!["/a.jpg".to_string(), "/b.jpg".to_string()]);
    }

    #[test]
    fn migration_v2_to_v3_adds_tags_preserves_data() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("v2cat.db");

        // Manually create a v2 schema (no tags yet)
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE files (
                  id INTEGER PRIMARY KEY,
                  path TEXT UNIQUE NOT NULL,
                  size_bytes INTEGER NOT NULL,
                  mtime_unix INTEGER NOT NULL,
                  exif_captured_unix INTEGER,
                  camera TEXT,
                  imported_at_unix INTEGER NOT NULL
                );
                CREATE TABLE file_hashes (
                  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                  sha256 TEXT,
                  dhash INTEGER
                );
                CREATE INDEX idx_files_path ON files(path);
                CREATE INDEX idx_files_exif_captured ON files(exif_captured_unix);
                CREATE INDEX idx_files_camera ON files(camera);
                "#,
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
            conn.execute(
                "INSERT INTO files (path, size_bytes, mtime_unix, exif_captured_unix, camera, imported_at_unix) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params!["/old2.jpg", 1234, 100, Some(200i64), Some("Nikon"), 300i64],
            ).unwrap();
            conn.execute(
                "INSERT INTO file_hashes (file_id, dhash) SELECT id, 99 FROM files WHERE path = ?1",
                rusqlite::params!["/old2.jpg"],
            )
            .unwrap();
        }

        // Open via Catalog — must upgrade to v3
        let cat = Catalog::open(&db_path).unwrap();

        let ver: i64 = cat
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(ver, 4);

        // Old row data intact
        let (size, mtime, cam): (i64, i64, Option<String>) = cat
            .conn
            .query_row(
                "SELECT size_bytes, mtime_unix, camera FROM files WHERE path = ?1",
                rusqlite::params!["/old2.jpg"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(size, 1234);
        assert_eq!(mtime, 100);
        assert_eq!(cam, Some("Nikon".to_string()));

        let d = cat.get_dhash("/old2.jpg", 100).unwrap();
        assert_eq!(d, Some(99u64));

        // Can now use tags
        cat.add_tag("/old2.jpg", "landscape", true, Some("user"))
            .unwrap();
        let tags = cat.get_tags("/old2.jpg").unwrap();
        assert_eq!(tags, vec![("landscape".to_string(), true)]);
    }

    #[test]
    fn migration_v3_to_v4_adds_meta_version_preimports_stale() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("v3cat.db");

        // Manually create a v3 schema (no meta_version column yet)
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE files (
                  id INTEGER PRIMARY KEY,
                  path TEXT UNIQUE NOT NULL,
                  size_bytes INTEGER NOT NULL,
                  mtime_unix INTEGER NOT NULL,
                  exif_captured_unix INTEGER,
                  camera TEXT,
                  imported_at_unix INTEGER NOT NULL
                );
                CREATE TABLE file_hashes (
                  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                  sha256 TEXT,
                  dhash INTEGER
                );
                CREATE TABLE tags (
                  id INTEGER PRIMARY KEY,
                  name TEXT NOT NULL UNIQUE COLLATE NOCASE
                );
                CREATE TABLE file_tags (
                  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                  tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
                  approved INTEGER NOT NULL DEFAULT 0,
                  source TEXT,
                  PRIMARY KEY (file_id, tag_id)
                );
                CREATE INDEX idx_files_path ON files(path);
                CREATE INDEX idx_files_exif_captured ON files(exif_captured_unix);
                CREATE INDEX idx_files_camera ON files(camera);
                CREATE INDEX idx_file_tags_tag ON file_tags(tag_id);
                CREATE INDEX idx_file_tags_approved ON file_tags(approved);
                "#,
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
            // Insert a row WITHOUT meta_version (v3 schema)
            conn.execute(
                "INSERT INTO files (path, size_bytes, mtime_unix, exif_captured_unix, camera, imported_at_unix) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params!["/premeta.jpg", 111, 222, Some(333i64), Some("Canon"), 444i64],
            ).unwrap();
        }

        // Open via Catalog — must upgrade to v4 and add meta_version
        let cat = Catalog::open(&db_path).unwrap();

        let ver: i64 = cat
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(ver, 4);

        // Column exists (querying succeeds) and old row's meta_version defaulted to 0
        let mv: i64 = cat
            .conn
            .query_row(
                "SELECT meta_version FROM files WHERE path = ?1",
                rusqlite::params!["/premeta.jpg"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mv, 0);

        // get_file_meta_fresh treats it as stale (meta_version 0 != META_VERSION)
        assert_eq!(cat.get_file_meta_fresh("/premeta.jpg", 222).unwrap(), None);
    }
}
