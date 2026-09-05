//! SQL-backed account + role repository (runtime-checked SQL).

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{
    AccountRepository, AuthError, IdentityRecord, NewAccount, UserActivity, UserSearch,
};
use bikesnest_domain::{
    AccountState, AuthenticationProvider, LocaleCode, Role, User, UserEmail, UserId,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

pub struct SqlxAccountRepository {
    db: Db,
}

impl SqlxAccountRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    async fn load_user(&self, row: UserRow) -> Result<User, AuthError> {
        let email = UserEmail::parse(&row.email).map_err(|_| AuthError::Internal)?;
        let account_state =
            AccountState::from_code(&row.account_state).ok_or(AuthError::Internal)?;
        let roles = self
            .roles_bysql(UserId(row.id))
            .await
            .map_err(|e| db_err("account.load_user", e))?;
        let mut user = User {
            id: UserId(row.id),
            email,
            display_name: row.display_name,
            account_state,
            email_verified_at: row.email_verified_at,
            roles,
            // The column is CHECK-constrained to the two codes `LocaleCode`
            // parses; an unparseable value would be a hand-edited row, and
            // falling back to the product default beats failing every read.
            locale: LocaleCode::parse(&row.locale).unwrap_or_default(),
        };
        if !user.roles.contains(&Role::User) {
            user.roles.push(Role::User);
        }
        Ok(user)
    }

    /// Hydrate a batch of rows. Roles are still one query per user (as they
    /// always were); the admin list is now a bounded page, so that is a fixed
    /// ≤50 lookups rather than one per account in the database.
    async fn load_users(&self, rows: Vec<UserRow>) -> Result<Vec<User>, AuthError> {
        let mut users = Vec::with_capacity(rows.len());
        for row in rows {
            users.push(self.load_user(row).await?);
        }
        Ok(users)
    }

    async fn roles_bysql(&self, id: UserId) -> Result<Vec<Role>, sqlx::Error> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT role FROM user_roles WHERE user_id = $1 ORDER BY role")
                .bind(id.0)
                .fetch_all(&mut *self.db.acquire().await?)
                .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(r,)| Role::from_code(&r))
            .collect())
    }
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: i64,
    email: String,
    display_name: Option<String>,
    account_state: String,
    email_verified_at: Option<DateTime<Utc>>,
    locale: String,
}

#[async_trait]
impl AccountRepository for SqlxAccountRepository {
    async fn public_contribution_name(&self, id: UserId) -> Result<bool, AuthError> {
        sqlx::query_scalar("SELECT public_contribution_name FROM users WHERE id = $1 AND account_state IN ('ACTIVE', 'PENDING_EMAIL_VERIFICATION')")
            .bind(id.0).fetch_optional(&mut *self.db.acquire().await.map_err(|e| db_err("account.acquire", e))?).await
            .map_err(|e| db_err("account.public_contribution_name", e))?
            .ok_or(AuthError::Unauthorized)
    }

    async fn set_public_contribution_name(
        &self,
        id: UserId,
        enabled: bool,
    ) -> Result<(), AuthError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("account.set_public_contribution_name", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("account.set_public_contribution_name", e))?;
        // Contributions capture consent under this same user-row lock. Revoking
        // and creating a review/proposal cannot cross and re-expose old content.
        let updated = sqlx::query("UPDATE users SET public_contribution_name = $2, public_contribution_name_updated_at = now() WHERE id = $1 AND account_state IN ('ACTIVE', 'PENDING_EMAIL_VERIFICATION')")
            .bind(id.0).bind(enabled).execute(&mut *tx).await
            .map_err(|e| db_err("account.set_public_contribution_name", e))?;
        if updated.rows_affected() != 1 {
            return Err(AuthError::Unauthorized);
        }
        if !enabled {
            sqlx::query(
                "UPDATE review SET public_author = FALSE WHERE author_id = $1 AND public_author",
            )
            .bind(id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("account.revoke_review_attribution", e))?;
            sqlx::query("UPDATE parking_proposal SET public_author = FALSE, public_author_revoked_at = now() WHERE proposer_id = $1 AND public_author")
                .bind(id.0).execute(&mut *tx).await
                .map_err(|e| db_err("account.revoke_proposal_attribution", e))?;
        }
        tx.commit()
            .await
            .map_err(|e| db_err("account.set_public_contribution_name", e))
    }
    async fn find_by_email(&self, email: &UserEmail) -> Result<Option<User>, AuthError> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, email, display_name, account_state, email_verified_at, locale
            FROM users
            WHERE lower(email) = $1
            "#,
        )
        .bind(email.as_str().to_lowercase())
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("account.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("account.find_by_email", e))?;
        match row {
            Some(row) => Ok(Some(self.load_user(row).await?)),
            None => Ok(None),
        }
    }

    async fn find_by_id(&self, id: UserId) -> Result<Option<User>, AuthError> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, email, display_name, account_state, email_verified_at, locale
            FROM users
            WHERE id = $1
            "#,
        )
        .bind(id.0)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("account.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("account.find_by_id", e))?;
        match row {
            Some(row) => Ok(Some(self.load_user(row).await?)),
            None => Ok(None),
        }
    }

    async fn create(&self, new: NewAccount<'_>) -> Result<UserId, AuthError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("account.create", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("account.create", e))?;
        let (id,): (i64,) = sqlx::query_as(
            r#"
            INSERT INTO users (email, display_name, account_state, locale, updated_at)
            VALUES ($1, $2, $3, $4, now())
            RETURNING id
            "#,
        )
        .bind(new.email.as_str())
        .bind(new.display_name)
        .bind(new.state.as_code())
        .bind(new.locale.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_err("account.create", e))?;

        if !new.password_hash.is_empty() {
            sqlx::query(
                r#"
                INSERT INTO authentication_identities
                    (user_id, provider, provider_subject, credential_hash)
                VALUES ($1, 'password', $2, $3)
                "#,
            )
            .bind(id)
            .bind(new.email.as_str())
            .bind(new.password_hash)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("account.create", e))?;
        }

        sqlx::query("INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, 'USER', NULL)")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("account.create", e))?;

        tx.commit().await.map_err(|e| db_err("account.create", e))?;
        Ok(UserId(id))
    }

    async fn set_state(&self, id: UserId, state: AccountState) -> Result<(), AuthError> {
        sqlx::query("UPDATE users SET account_state = $2, updated_at = now() WHERE id = $1")
            .bind(id.0)
            .bind(state.as_code())
            .execute(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("account.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("account.set_state", e))?;
        Ok(())
    }

    async fn mark_email_verified(&self, id: UserId, at: DateTime<Utc>) -> Result<(), AuthError> {
        sqlx::query("UPDATE users SET email_verified_at = $2, updated_at = now() WHERE id = $1")
            .bind(id.0)
            .bind(at)
            .execute(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("account.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("account.mark_email_verified", e))?;
        Ok(())
    }

    async fn update_canonical_email(&self, id: UserId, email: &UserEmail) -> Result<(), AuthError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("account.update_canonical_email", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("account.update_canonical_email", e))?;
        sqlx::query("UPDATE users SET email = $2, updated_at = now() WHERE id = $1")
            .bind(id.0)
            .bind(email.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("account.update_canonical_email", e))?;
        // Keep the password identity's subject in sync ( one login-lookup key).
        sqlx::query(
            "UPDATE authentication_identities SET provider_subject = $2
             WHERE user_id = $1 AND provider = 'password'",
        )
        .bind(id.0)
        .bind(email.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("account.update_canonical_email", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("account.update_canonical_email", e))?;
        Ok(())
    }

    async fn confirm_email(
        &self,
        id: UserId,
        at: DateTime<Utc>,
        email: &UserEmail,
    ) -> Result<(), AuthError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("account.confirm_email", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("account.confirm_email", e))?;
        // email_verified_at + advance to Active + (if different) switch the
        // canonical email, all in one transaction.
        sqlx::query(
            "UPDATE users SET email = $2, email_verified_at = $3, account_state = 'ACTIVE',
             updated_at = now() WHERE id = $1",
        )
        .bind(id.0)
        .bind(email.as_str())
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("account.confirm_email", e))?;
        // Keep the password identity's subject in sync with the canonical email.
        sqlx::query(
            "UPDATE authentication_identities SET provider_subject = $2
             WHERE user_id = $1 AND provider = 'password'",
        )
        .bind(id.0)
        .bind(email.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("account.confirm_email", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("account.confirm_email", e))?;
        Ok(())
    }

    async fn set_password(&self, id: UserId, hash: &str) -> Result<(), AuthError> {
        sqlx::query(
            "UPDATE authentication_identities SET credential_hash = $2
             WHERE user_id = $1 AND provider = 'password'",
        )
        .bind(id.0)
        .bind(hash)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("account.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("account.set_password", e))?;
        Ok(())
    }

    async fn set_locale(&self, id: UserId, locale: LocaleCode) -> Result<(), AuthError> {
        sqlx::query("UPDATE users SET locale = $2, updated_at = now() WHERE id = $1")
            .bind(id.0)
            .bind(locale.as_str())
            .execute(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("account.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("account.set_locale", e))?;
        Ok(())
    }

    async fn link_identity(
        &self,
        user_id: UserId,
        provider: AuthenticationProvider,
        subject: &str,
        hash: Option<&str>,
    ) -> Result<(), AuthError> {
        sqlx::query(
            r#"
            INSERT INTO authentication_identities
                (user_id, provider, provider_subject, credential_hash)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (provider, provider_subject) DO NOTHING
            "#,
        )
        .bind(user_id.0)
        .bind(provider.as_code())
        .bind(subject)
        .bind(hash)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("account.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("account.link_identity", e))?;
        Ok(())
    }

    async fn find_identity(
        &self,
        provider: AuthenticationProvider,
        subject: &str,
    ) -> Result<Option<IdentityRecord>, AuthError> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: i64,
            user_id: i64,
            provider: String,
            provider_subject: String,
            credential_hash: Option<String>,
        }
        let row = sqlx::query_as::<_, Row>(
            r#"
            SELECT id, user_id, provider, provider_subject, credential_hash
            FROM authentication_identities
            WHERE provider = $1 AND provider_subject = $2
            "#,
        )
        .bind(provider.as_code())
        .bind(subject)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("account.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("account.find_identity", e))?;
        match row {
            Some(r) => {
                let provider =
                    AuthenticationProvider::from_code(&r.provider).ok_or(AuthError::Internal)?;
                Ok(Some(IdentityRecord {
                    id: r.id,
                    user_id: UserId(r.user_id),
                    provider,
                    provider_subject: r.provider_subject,
                    credential_hash: r.credential_hash,
                }))
            }
            None => Ok(None),
        }
    }

    async fn roles(&self, id: UserId) -> Result<Vec<Role>, AuthError> {
        self.roles_bysql(id)
            .await
            .map_err(|e| db_err("account.roles", e))
    }

    async fn count_admins(&self) -> Result<i64, AuthError> {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_roles WHERE role = 'ADMIN'")
            .fetch_one(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("account.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("account.count_admins", e))
    }

    async fn grant_role(&self, id: UserId, role: Role, by: UserId) -> Result<(), AuthError> {
        sqlx::query(
            "INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, role) DO NOTHING",
        )
        .bind(id.0)
        .bind(role.as_code())
        .bind(by.0)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("account.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("account.grant_role", e))?;
        Ok(())
    }

    async fn revoke_role_guarded(&self, id: UserId, role: Role) -> Result<bool, AuthError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("account.revoke_role_guarded", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("account.revoke_role_guarded", e))?;

        // `FOR UPDATE` on the ADMIN rows is what makes the guard hold: two
        // concurrent revokes serialize here, so the second one sees the first
        // one's delete instead of the pre-delete count.
        let admins: Vec<i64> = sqlx::query_scalar::<_, i64>(
            "SELECT user_id FROM user_roles WHERE role = 'ADMIN' FOR UPDATE",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("account.revoke_role_guarded", e))?;

        // Stripping a role the target does not hold removes no admin, so it is
        // a no-op and stays allowed even at a count of one.
        if role == Role::Admin && admins.len() <= 1 && admins.contains(&id.0) {
            return Err(AuthError::RefuseAdminSelfRevoke);
        }

        let res = sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role = $2")
            .bind(id.0)
            .bind(role.as_code())
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("account.revoke_role_guarded", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("account.revoke_role_guarded", e))?;
        Ok(res.rows_affected() > 0)
    }

    async fn list_users(&self) -> Result<Vec<User>, AuthError> {
        let rows = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, email, display_name, account_state, email_verified_at, locale
            FROM users
            ORDER BY id DESC
            "#,
        )
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("account.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("account.list_users", e))?;
        self.load_users(rows).await
    }

    async fn search_users(&self, search: UserSearch<'_>) -> Result<Vec<User>, AuthError> {
        // `ILIKE` with a bound `%term%`: the wildcards are added here, the
        // term itself is never concatenated into the SQL.
        let pattern = search.query.map(|q| format!("%{}%", escape_like(q)));
        let rows = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, email, display_name, account_state, email_verified_at, locale
            FROM users
            WHERE ($1::text IS NULL OR email ILIKE $1::text ESCAPE '!'
                                    OR display_name ILIKE $1::text ESCAPE '!')
              AND ($2::bigint IS NULL OR id < $2::bigint)
            ORDER BY id DESC
            LIMIT $3
            "#,
        )
        .bind(pattern.as_deref())
        .bind(search.after_id)
        .bind(search.limit.clamp(1, 200))
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("account.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("account.search_users", e))?;
        self.load_users(rows).await
    }

    async fn labels_for(&self, ids: &[i64]) -> Result<HashMap<i64, String>, AuthError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(i64, String)> = sqlx::query_as(
            r#"
            SELECT id, coalesce(nullif(btrim(display_name), ''), email) AS label
            FROM users
            WHERE id = ANY($1)
            "#,
        )
        .bind(ids)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("account.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("account.labels_for", e))?;
        Ok(rows.into_iter().collect())
    }

    async fn activity_for(&self, ids: &[i64]) -> Result<HashMap<i64, UserActivity>, AuthError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query_as::<_, ActivityRow>(ACTIVITY_SQL)
            .bind(ids)
            .fetch_all(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("account.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("account.activity_for", e))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.user_id,
                    UserActivity {
                        last_active_at: r.last_active_at,
                        contributions: r.contributions.unwrap_or(0),
                    },
                )
            })
            .collect())
    }
}

/// Last-seen plus one contribution total per account, for a whole page of the
/// admin user list in one round trip. The counted events are the ones the C5
/// contribution feed lists, so the number on the admin row and the number of
/// rows on the user's own history page agree.
const ACTIVITY_SQL: &str = r#"
WITH ids AS (SELECT unnest($1::bigint[]) AS user_id),
last_seen AS (
    SELECT s.user_id, max(s.last_seen_at) AS last_active_at
    FROM sessions s
    JOIN ids ON ids.user_id = s.user_id
    GROUP BY s.user_id
),
contributions AS (
    SELECT user_id, sum(n)::bigint AS n FROM (
        SELECT creator_id AS user_id, count(*) AS n FROM parking_location
            WHERE creator_id = ANY($1) GROUP BY creator_id
        UNION ALL
        SELECT editor_id, count(*) FROM parking_revision
            WHERE editor_id = ANY($1) AND change_kind = 'edit' GROUP BY editor_id
        UNION ALL
        SELECT proposer_id, count(*) FROM parking_proposal
            WHERE proposer_id = ANY($1) GROUP BY proposer_id
        UNION ALL
        SELECT author_id, count(*) FROM review
            WHERE author_id = ANY($1) GROUP BY author_id
        UNION ALL
        SELECT user_id, count(*) FROM verification
            WHERE user_id = ANY($1) GROUP BY user_id
        UNION ALL
        SELECT uploader_id, count(*) FROM parking_photo
            WHERE uploader_id = ANY($1) GROUP BY uploader_id
        UNION ALL
        SELECT uploader_id, count(*) FROM review_photo
            WHERE uploader_id = ANY($1) GROUP BY uploader_id
    ) parts
    GROUP BY user_id
)
SELECT ids.user_id, last_seen.last_active_at, contributions.n AS contributions
FROM ids
LEFT JOIN last_seen ON last_seen.user_id = ids.user_id
LEFT JOIN contributions ON contributions.user_id = ids.user_id
"#;

#[derive(sqlx::FromRow)]
struct ActivityRow {
    user_id: i64,
    last_active_at: Option<DateTime<Utc>>,
    contributions: Option<i64>,
}

/// Neutralize `%`, `_` and `\` so a search for "a_b" does not match "axb".
/// Escapes LIKE metacharacters with `!`, matching the `ESCAPE '!'` clause the
/// search query declares, so a literal `%` or `_` in the term matches itself.
fn escape_like(term: &str) -> String {
    term.replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// [`AuthError`]. `context` names the operation, e.g. `"account.create"`.
fn db_err(context: &'static str, e: sqlx::Error) -> AuthError {
    crate::db_error::classify_and_log(context, e).into()
}

#[cfg(test)]
mod like_tests {
    use super::escape_like;

    #[test]
    fn like_metacharacters_are_escaped_with_the_declared_escape_char() {
        assert_eq!(escape_like("a_b%c!"), "a!_b!%c!!");
        assert_eq!(escape_like("plain"), "plain");
    }
}
