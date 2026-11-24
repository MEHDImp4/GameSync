use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};
use uuid::Uuid;

pub struct Devices(Connection);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRequest {
    pub code: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct Credentials {
    pub device: Uuid,
    pub token: String,
}

#[derive(Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub administrator: bool,
    pub revoked: bool,
    pub last_seen: Option<i64>,
}

fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn digest(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex().to_string()
}

impl Devices {
    pub fn open(data: &Path) -> rusqlite::Result<Self> {
        let db = Connection::open(data.join("devices.sqlite"))?;
        db.busy_timeout(Duration::from_secs(5))?;
        Ok(Self(db))
    }

    pub fn initialize(&mut self) -> rusqlite::Result<()> {
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS invitations (digest TEXT PRIMARY KEY, expires INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS devices (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL, digest TEXT UNIQUE NOT NULL,
                 revoked INTEGER NOT NULL DEFAULT 0
             );",
        )?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            transaction.execute_batch(
                "ALTER TABLE invitations ADD COLUMN administrator INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE devices ADD COLUMN administrator INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE devices ADD COLUMN last_seen INTEGER;
                 PRAGMA user_version = 1;",
            )?;
        }
        if version < 2 {
            transaction.execute_batch(
                "CREATE TABLE retention (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    keep_count INTEGER NOT NULL CHECK (keep_count > 0),
                    keep_days INTEGER NOT NULL CHECK (keep_days > 0)
                 );
                 PRAGMA user_version = 2;",
            )?;
        }
        transaction.commit()
    }

    pub fn retention(&self, defaults: (u32, u32)) -> rusqlite::Result<(u32, u32)> {
        Ok(self
            .0
            .query_row(
                "SELECT keep_count, keep_days FROM retention WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or(defaults))
    }

    pub fn set_retention(&self, actor: Uuid, count: u32, days: u32) -> rusqlite::Result<bool> {
        let updated = self.0.execute(
            "INSERT INTO retention (id, keep_count, keep_days)
             SELECT 1, ?, ? FROM devices WHERE id = ? AND administrator = 1 AND revoked = 0
             ON CONFLICT(id) DO UPDATE SET keep_count = excluded.keep_count, keep_days = excluded.keep_days",
            params![count, days, actor.to_string()],
        )?;
        Ok(updated != 0)
    }

    pub fn invite(&self, administrator: bool) -> rusqlite::Result<String> {
        let code = secret();
        self.0
            .execute("DELETE FROM invitations WHERE expires <= unixepoch()", [])?;
        self.0.execute(
            "INSERT INTO invitations (digest, expires, administrator) VALUES (?, unixepoch() + 600, ?)",
            params![digest(&code), administrator],
        )?;
        Ok(code)
    }

    pub fn pair(&mut self, invite: &str, name: &str) -> rusqlite::Result<Option<Credentials>> {
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let administrator = transaction.query_row(
            "DELETE FROM invitations WHERE digest = ? AND expires > unixepoch() RETURNING administrator",
            [digest(invite)],
            |row| row.get::<_, bool>(0),
        ).optional()?;
        let Some(administrator) = administrator else {
            return Ok(None);
        };

        let device = Uuid::new_v4();
        let token = secret();
        transaction.execute(
            "INSERT INTO devices (id, name, digest, administrator, last_seen) VALUES (?, ?, ?, ?, unixepoch())",
            params![device.to_string(), name, digest(&token), administrator],
        )?;
        transaction.commit()?;
        Ok(Some(Credentials { device, token }))
    }

    pub fn authenticate(&self, token: &str) -> rusqlite::Result<Option<Uuid>> {
        let token = digest(token);
        let row: Option<(String, bool)> = self
            .0
            .query_row(
                "SELECT id, last_seen IS NULL OR last_seen < unixepoch() - 60
             FROM devices WHERE digest = ? AND revoked = 0",
                [token],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((id, should_update)) = row else {
            return Ok(None);
        };
        if should_update {
            self.0.execute(
                "UPDATE devices SET last_seen = unixepoch() WHERE id = ? AND revoked = 0",
                [&id],
            )?;
        }
        Ok(Uuid::parse_str(&id).ok())
    }

    pub fn list(&self) -> rusqlite::Result<Vec<Device>> {
        let mut query = self.0.prepare(
            "SELECT id, name, administrator, revoked, last_seen FROM devices ORDER BY revoked, name, id",
        )?;
        query
            .query_map([], |row| {
                Ok(Device {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    administrator: row.get(2)?,
                    revoked: row.get(3)?,
                    last_seen: row.get(4)?,
                })
            })?
            .collect()
    }

    pub fn is_admin(&self, actor: Uuid) -> rusqlite::Result<bool> {
        self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id = ? AND administrator = 1 AND revoked = 0)",
            [actor.to_string()], |row| row.get(0),
        )
    }

    pub fn invite_as(&mut self, actor: Uuid) -> rusqlite::Result<Option<String>> {
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let code = secret();
        let inserted = transaction.execute(
            "INSERT INTO invitations (digest, expires, administrator)
             SELECT ?, unixepoch() + 600, 0 FROM devices
             WHERE id = ? AND administrator = 1 AND revoked = 0",
            params![digest(&code), actor.to_string()],
        )?;
        transaction.execute("DELETE FROM invitations WHERE expires <= unixepoch()", [])?;
        transaction.commit()?;
        Ok((inserted != 0).then_some(code))
    }

    pub fn revoke_as(&mut self, actor: Uuid, target: Uuid) -> rusqlite::Result<Option<bool>> {
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let allowed: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id = ? AND administrator = 1 AND revoked = 0)",
            [actor.to_string()], |row| row.get(0),
        )?;
        if !allowed || actor == target {
            return Ok(None);
        }
        let updated = transaction.execute(
            "UPDATE devices SET revoked = 1 WHERE id = ?",
            [target.to_string()],
        )?;
        transaction.commit()?;
        Ok(Some(updated != 0))
    }

    pub fn revoke(&self, device: Uuid) -> rusqlite::Result<bool> {
        Ok(self.0.execute(
            "UPDATE devices SET revoked = 1 WHERE id = ?",
            [device.to_string()],
        )? != 0)
    }
}
