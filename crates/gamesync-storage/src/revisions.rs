use crate::{ObjectError, ObjectStore};
use gamesync_core::{GameRegistration, Manifest};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    collections::HashSet,
    path::Path,
    time::{Duration, SystemTime},
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RevisionError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error("revision does not exist for this game")]
    UnknownRevision,
    #[error("invalid game registration")]
    InvalidGame,
    #[error("save roots differ from the registered game")]
    GameRootsMismatch,
    #[error("revision contents expired under retention")]
    ExpiredRevision,
    #[error("publication identifier is already associated with another request")]
    PublicationMismatch,
    #[error("the selected conflict is no longer current")]
    StaleResolution,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Publication {
    Head(i64),
    Conflict(i64),
}

#[derive(Debug)]
pub struct Revision {
    pub id: i64,
    pub machine: String,
    pub created_at: i64,
    pub manifest: Manifest,
    pub published: bool,
    pub resolved_by: Option<i64>,
}

#[derive(Debug)]
pub struct RevisionSummary {
    pub id: i64,
    pub machine: String,
    pub created_at: i64,
    pub published: bool,
    pub resolved_by: Option<i64>,
    pub bytes: u64,
}

#[derive(Debug, Default)]
pub struct Cleanup {
    pub expired_revisions: usize,
    pub removed_objects: usize,
}

#[derive(Debug)]
pub struct RecentPublication {
    pub revision: i64,
    pub game: String,
    pub title: Option<String>,
    pub machine: String,
    pub created_at: i64,
}

pub struct PublishInput<'a> {
    pub game: Uuid,
    pub machine: Uuid,
    pub request: Uuid,
    pub base: Option<i64>,
    pub manifest: &'a Manifest,
    pub resolves: Option<i64>,
}

pub struct Revisions(Connection);

impl Revisions {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RevisionError> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(include_str!("revisions.sql"))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 2 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let version: i64 =
                transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if version == 0 {
                transaction.execute_batch(
                    "ALTER TABLE revisions ADD COLUMN expired_digest TEXT;
                     CREATE INDEX IF NOT EXISTS revisions_game_revision ON revisions(game, id DESC);
                     PRAGMA user_version = 1;",
                )?;
            }
            if version < 2 {
                transaction.execute_batch(
                    "ALTER TABLE revisions ADD COLUMN resolves INTEGER REFERENCES revisions(id);
                     ALTER TABLE revisions ADD COLUMN resolved_by INTEGER REFERENCES revisions(id);
                     PRAGMA user_version = 2;",
                )?;
            }
            transaction.commit()?;
        }
        Ok(Self(connection))
    }

    pub fn games(&self) -> Result<Vec<GameRegistration>, RevisionError> {
        let mut statement = self.0.prepare(
            "SELECT registration FROM games ORDER BY json_extract(registration, '$.title') COLLATE NOCASE, id",
        )?;
        let records = statement.query_map([], |row| row.get::<_, String>(0))?;
        records
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    pub fn register_game(&mut self, registration: &GameRegistration) -> Result<(), RevisionError> {
        if !registration.is_valid() {
            return Err(RevisionError::InvalidGame);
        }
        let game = registration.id.to_string();
        let ids = registration.root_ids();
        let existing_roots = |serialized: String| -> Result<(), RevisionError> {
            let existing: GameRegistration = serde_json::from_str(&serialized)?;
            if existing.root_ids() != ids {
                return Err(RevisionError::GameRootsMismatch);
            }
            Ok(())
        };

        if let Some(existing) = read_registration(&self.0, &game)? {
            return existing_roots(existing);
        }
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = read_registration(&transaction, &game)? {
            return existing_roots(existing);
        }
        // Existing repositories may have revisions from before the catalog was introduced.
        {
            let mut statement = transaction.prepare(
                "SELECT DISTINCT json_extract(entry.value, '$.root')
                 FROM revisions, json_each(revisions.manifest) AS entry
                 WHERE revisions.game = ? AND revisions.expired_digest IS NULL",
            )?;
            let used = statement.query_map([&game], |row| row.get::<_, String>(0))?;
            for used in used {
                if !Uuid::parse_str(&used?).is_ok_and(|root| ids.contains(&root)) {
                    return Err(RevisionError::GameRootsMismatch);
                }
            }
        }
        transaction.execute(
            "INSERT INTO games (id, registration) VALUES (?, ?)",
            params![game, serde_json::to_string(registration)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn synchronized_count(&self) -> Result<i64, RevisionError> {
        Ok(self
            .0
            .query_row("SELECT count(*) FROM heads", [], |row| row.get(0))?)
    }

    pub fn recent_publications(&self) -> Result<Vec<RecentPublication>, RevisionError> {
        let mut statement = self.0.prepare(
            "SELECT revisions.id, revisions.game, json_extract(games.registration, '$.title'),
                    revisions.machine, revisions.created_at
             FROM revisions LEFT JOIN games ON games.id = revisions.game
             WHERE revisions.published = 1 ORDER BY revisions.id DESC LIMIT 10",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(RecentPublication {
                    revision: row.get(0)?,
                    game: row.get(1)?,
                    title: row.get(2)?,
                    machine: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    pub fn head(&self, game: Uuid) -> Result<Option<i64>, RevisionError> {
        Ok(self
            .0
            .query_row(
                "SELECT revision FROM heads WHERE game = ?",
                [game.to_string()],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn get(&self, game: Uuid, id: i64) -> Result<Revision, RevisionError> {
        let (machine, created_at, manifest, expired, published, resolved_by): (String, i64, String, bool, bool, Option<i64>) = self
            .0
            .query_row(
                "SELECT machine, created_at, manifest, expired_digest IS NOT NULL, published, resolved_by FROM revisions WHERE game = ? AND id = ?",
                params![game.to_string(), id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()?
            .ok_or(RevisionError::UnknownRevision)?;
        if expired {
            return Err(RevisionError::ExpiredRevision);
        }
        Ok(Revision {
            id,
            machine,
            created_at,
            manifest: serde_json::from_str(&manifest)?,
            published,
            resolved_by,
        })
    }

    pub fn history(&self, game: Uuid, before: i64) -> Result<Vec<RevisionSummary>, RevisionError> {
        let mut query = self.0.prepare(
            "SELECT id, machine, created_at, published, manifest, resolved_by FROM revisions
             WHERE game = ? AND id < ? AND expired_digest IS NULL ORDER BY id DESC LIMIT 50",
        )?;
        let mut rows = query.query(params![game.to_string(), before])?;
        let mut revisions = Vec::new();
        while let Some(row) = rows.next()? {
            let manifest: Manifest = serde_json::from_str(&row.get::<_, String>(4)?)?;
            revisions.push(RevisionSummary {
                id: row.get(0)?,
                machine: row.get(1)?,
                created_at: row.get(2)?,
                published: row.get(3)?,
                resolved_by: row.get(5)?,
                bytes: manifest
                    .entries()
                    .iter()
                    .fold(0_u64, |total, entry| total.saturating_add(entry.size)),
            });
        }
        Ok(revisions)
    }

    /// Expire old snapshot contents while retaining revision IDs and idempotency records.
    /// Conflict variants and their bases are kept until explicitly resolved.
    pub fn cleanup(
        &mut self,
        objects: &ObjectStore,
        keep_count: u32,
        keep_days: u32,
        grace: Duration,
    ) -> Result<Cleanup, RevisionError> {
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = {
            let mut query = transaction.prepare(
                "WITH ranked AS (
                    SELECT r.id, max(r.created_at, coalesce(resolution.created_at, 0)) AS created_at, r.manifest, r.published,
                           row_number() OVER (PARTITION BY r.game, r.published ORDER BY r.id DESC) AS rank
                    FROM revisions r LEFT JOIN revisions resolution ON resolution.id = r.resolved_by
                    WHERE (r.published = 1 OR r.resolved_by IS NOT NULL) AND r.expired_digest IS NULL
                 )
                 SELECT id, manifest FROM ranked
                 WHERE (published = 0 OR rank > ?) AND created_at < unixepoch() - ?
                   AND id NOT IN (SELECT revision FROM heads)
                   AND id NOT IN (SELECT base FROM revisions WHERE published = 0 AND resolved_by IS NULL AND base IS NOT NULL)"
            )?;
            query
                .query_map(params![keep_count, i64::from(keep_days) * 86400], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (id, manifest) in &candidates {
            transaction.execute(
                "UPDATE revisions SET manifest = '[]', expired_digest = ? WHERE id = ?",
                params![manifest_digest(manifest), id],
            )?;
        }
        transaction.commit()?;

        // Re-read references while publication is locked. If object cleanup fails, expired
        // manifests stay expired, and a later run can retry collecting orphaned files.
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let referenced = {
            let mut query = transaction.prepare(
                "SELECT DISTINCT json_extract(entry.value, '$.digest')
                 FROM revisions, json_each(revisions.manifest) AS entry
                 WHERE revisions.expired_digest IS NULL",
            )?;
            query
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<HashSet<_>, _>>()?
        };
        let removed_objects = objects.remove_unreferenced(&referenced, SystemTime::now(), grace)?;
        transaction.commit()?;
        Ok(Cleanup {
            expired_revisions: candidates.len(),
            removed_objects,
        })
    }

    pub fn publish(
        &mut self,
        objects: &ObjectStore,
        game: Uuid,
        machine: Uuid,
        request: Uuid,
        base: Option<i64>,
        manifest: &Manifest,
    ) -> Result<Publication, RevisionError> {
        self.publish_with_resolution(
            objects,
            PublishInput {
                game,
                machine,
                request,
                base,
                manifest,
                resolves: None,
            },
        )
    }

    pub fn publish_with_resolution(
        &mut self,
        objects: &ObjectStore,
        input: PublishInput<'_>,
    ) -> Result<Publication, RevisionError> {
        let PublishInput {
            game,
            machine,
            request,
            base,
            manifest,
            resolves,
        } = input;
        let game = game.to_string();
        let machine = machine.to_string();
        let request = request.to_string();
        let serialized = serde_json::to_string(manifest)?;
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = transaction.query_row(
            "SELECT id, base, machine, manifest, published, expired_digest, resolves FROM revisions WHERE game = ? AND request = ?",
            params![game, request],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, bool>(4)?, row.get::<_, Option<String>>(5)?, row.get::<_, Option<i64>>(6)?)),
        ).optional()?;
        if let Some((id, old_base, old_machine, old_manifest, published, expired, old_resolves)) =
            previous
        {
            let matches = expired.map_or_else(
                || old_manifest == serialized,
                |digest| digest == manifest_digest(&serialized),
            );
            if old_base != base || old_machine != machine || old_resolves != resolves || !matches {
                return Err(RevisionError::PublicationMismatch);
            }
            return Ok(if published {
                Publication::Head(id)
            } else {
                Publication::Conflict(id)
            });
        }
        if let Some(id) = base {
            transaction
                .query_row(
                    "SELECT id FROM revisions WHERE game = ? AND id = ? AND published = 1",
                    params![game, id],
                    |_| Ok(()),
                )
                .optional()?
                .ok_or(RevisionError::UnknownRevision)?;
        }
        let head = transaction
            .query_row(
                "SELECT revision FROM heads WHERE game = ?",
                [&game],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(value) = read_registration(&transaction, &game)? {
            let registered: GameRegistration = serde_json::from_str(&value)?;
            let roots = registered.root_ids();
            if manifest
                .entries()
                .iter()
                .any(|entry| !roots.contains(&entry.root))
            {
                return Err(RevisionError::GameRootsMismatch);
            }
        }
        if let Some(variant) = resolves {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM revisions WHERE game = ? AND id = ?
                   AND published = 0 AND resolved_by IS NULL AND expired_digest IS NULL)",
                params![game, variant],
                |row| row.get(0),
            )?;
            if !exists || head.is_none() || head != base {
                return Err(RevisionError::StaleResolution);
            }
        }
        let published = head == base;
        for file in manifest.entries() {
            objects.open_verified(&file.digest, file.size)?;
        }
        transaction.execute(
            "INSERT INTO revisions (game, machine, request, base, manifest, published, resolves) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![game, machine, request, base, serialized, published, resolves],
        )?;
        let id = transaction.last_insert_rowid();
        if published {
            transaction.execute(
                "INSERT INTO heads (game, revision) VALUES (?, ?) ON CONFLICT(game) DO UPDATE SET revision = excluded.revision",
                params![game, id],
            )?;
        }
        if let Some(variant) = resolves {
            transaction.execute(
                "UPDATE revisions SET resolved_by = ? WHERE id = ?",
                params![id, variant],
            )?;
        }
        transaction.commit()?;
        Ok(if published {
            Publication::Head(id)
        } else {
            Publication::Conflict(id)
        })
    }
}

fn manifest_digest(manifest: &str) -> String {
    blake3::hash(manifest.as_bytes()).to_hex().to_string()
}

fn read_registration(connection: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    connection
        .query_row("SELECT registration FROM games WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .optional()
}
