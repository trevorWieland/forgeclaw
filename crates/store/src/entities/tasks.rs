//! SeaORM entity backing the `tasks` table.

use sea_orm::entity::prelude::*;

/// Row model for `tasks`.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "tasks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub group_id: String,
    pub prompt: String,
    pub schedule_kind: String,
    pub schedule_value: String,
    pub status: String,
    pub next_run: Option<DateTimeUtc>,
    pub last_result: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
