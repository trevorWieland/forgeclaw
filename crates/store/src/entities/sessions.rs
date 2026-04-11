//! SeaORM entity backing the `sessions` table (group → agent session id).

use sea_orm::entity::prelude::*;

/// Row model for `sessions`.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub group_id: String,
    pub session_id: String,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
