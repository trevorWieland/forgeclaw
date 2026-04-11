//! SeaORM entity backing the `state` key/value table.

use sea_orm::entity::prelude::*;

/// Row model for `state`.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    pub value: String,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
