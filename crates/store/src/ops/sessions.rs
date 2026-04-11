//! Persistence ops for the `sessions` table (group → agent session id).

use chrono::Utc;
use forgeclaw_core::id::GroupId;
use sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait, sea_query::OnConflict};

use crate::entities::sessions;
use crate::error::StoreError;

/// Fetch the current session id for a group.
pub(crate) async fn get_session(
    db: &DatabaseConnection,
    group: &GroupId,
) -> Result<Option<String>, StoreError> {
    let row = sessions::Entity::find_by_id(group.as_ref().to_owned())
        .one(db)
        .await?;
    Ok(row.map(|m| m.session_id))
}

/// Insert or update the session id for a group.
pub(crate) async fn set_session(
    db: &DatabaseConnection,
    group: &GroupId,
    session_id: &str,
) -> Result<(), StoreError> {
    let am = sessions::ActiveModel {
        group_id: Set(group.as_ref().to_owned()),
        session_id: Set(session_id.to_owned()),
        updated_at: Set(Utc::now()),
    };
    sessions::Entity::insert(am)
        .on_conflict(
            OnConflict::column(sessions::Column::GroupId)
                .update_columns([sessions::Column::SessionId, sessions::Column::UpdatedAt])
                .to_owned(),
        )
        .exec(db)
        .await?;
    Ok(())
}
