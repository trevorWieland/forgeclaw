//! Persistence ops for the `groups` table.

use forgeclaw_core::id::GroupId;
use sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait, sea_query::OnConflict};

use crate::entities::groups;
use crate::error::StoreError;
use crate::types::group::RegisteredGroup;

/// Fetch a group by id.
pub(crate) async fn get_group(
    db: &DatabaseConnection,
    id: &GroupId,
) -> Result<Option<RegisteredGroup>, StoreError> {
    let row = groups::Entity::find_by_id(id.as_ref().to_owned())
        .one(db)
        .await?;
    row.map(row_to_registered).transpose()
}

/// Upsert a group, preserving `created_at` on conflict.
pub(crate) async fn upsert_group(
    db: &DatabaseConnection,
    group: &RegisteredGroup,
) -> Result<(), StoreError> {
    let am = groups::ActiveModel {
        id: Set(group.id.as_ref().to_owned()),
        display_name: Set(group.display_name.clone()),
        config_json: Set(group.config_json.clone()),
        active: Set(group.active),
        created_at: Set(group.created_at),
        updated_at: Set(group.updated_at),
    };
    groups::Entity::insert(am)
        .on_conflict(
            OnConflict::column(groups::Column::Id)
                .update_columns([
                    groups::Column::DisplayName,
                    groups::Column::ConfigJson,
                    groups::Column::Active,
                    groups::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec(db)
        .await?;
    Ok(())
}

fn row_to_registered(row: groups::Model) -> Result<RegisteredGroup, StoreError> {
    Ok(RegisteredGroup {
        id: GroupId::new(row.id).map_err(|e| StoreError::SchemaDrift {
            table: Some("groups".to_owned()),
            column: Some("id".to_owned()),
            reason: e.to_string(),
        })?,
        display_name: row.display_name,
        config_json: row.config_json,
        active: row.active,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}
