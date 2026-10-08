//! Destructive reset used only by the `seed-full-fresh` development command.

use crate::Db;
use bikesnest_application::{ObjectStorage, StorageError};

#[derive(Debug, thiserror::Error)]
pub enum FreshSeedResetError {
    #[error("database reset failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("object-storage reset failed: {0}")]
    Storage(#[from] StorageError),
}

/// Delete every object in the configured bucket and truncate all mutable
/// application data. Schema migrations and the security-feature catalog stay
/// intact.
pub async fn reset_all_data(
    db: &Db,
    storage: &dyn ObjectStorage,
) -> Result<usize, FreshSeedResetError> {
    let mut deleted_objects = 0;
    let mut after = None;
    loop {
        let page = storage.list("", after.as_deref()).await?;
        for object in &page.objects {
            storage.delete(&object.key).await?;
            deleted_objects += 1;
        }
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }

    let mut conn = db.acquire().await?;
    sqlx::query(
        r#"
        TRUNCATE TABLE
            background_job,
            policy_version,
            consent_record,
            personal_data_export,
            privacy_request,
            review_photo,
            report,
            favorite,
            verification,
            review_revision,
            review,
            parking_proposal,
            parking_revision,
            audit_events,
            user_roles,
            password_reset_tokens,
            email_verification_tokens,
            sessions,
            authentication_identities,
            parking_photo,
            opening_hours,
            parking_security,
            parking_location,
            users
        RESTART IDENTITY CASCADE
        "#,
    )
    .execute(&mut *conn)
    .await?;

    Ok(deleted_objects)
}
