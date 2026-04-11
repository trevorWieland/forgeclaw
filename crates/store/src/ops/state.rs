//! Persistence ops for the `state` key/value table.

use chrono::Utc;
use sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait, sea_query::OnConflict};

use crate::entities::state;
use crate::error::StoreError;

/// Fetch a state value by key.
pub(crate) async fn get_state(
    db: &DatabaseConnection,
    key: &str,
) -> Result<Option<String>, StoreError> {
    let row = state::Entity::find_by_id(key.to_owned()).one(db).await?;
    Ok(row.map(|m| m.value))
}

/// Insert or update a state value.
pub(crate) async fn set_state(
    db: &DatabaseConnection,
    key: &str,
    value: &str,
) -> Result<(), StoreError> {
    let am = state::ActiveModel {
        key: Set(key.to_owned()),
        value: Set(value.to_owned()),
        updated_at: Set(Utc::now()),
    };
    state::Entity::insert(am)
        .on_conflict(
            OnConflict::column(state::Column::Key)
                .update_columns([state::Column::Value, state::Column::UpdatedAt])
                .to_owned(),
        )
        .exec(db)
        .await?;
    Ok(())
}
