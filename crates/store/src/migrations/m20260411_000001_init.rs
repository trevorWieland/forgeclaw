//! Initial schema: groups, messages, tasks, state, sessions, events.
//!
//! Written once with SeaQuery's `SchemaManager` so the same definition
//! emits correct DDL for both SQLite and PostgreSQL. Indexes include a
//! composite cursor index for message pagination and a partial index for
//! the active-task due-query.

use sea_orm_migration::prelude::*;

#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        create_groups(manager).await?;
        create_messages(manager).await?;
        create_tasks(manager).await?;
        create_state(manager).await?;
        create_sessions(manager).await?;
        create_events(manager).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop in reverse creation order.
        manager
            .drop_table(Table::drop().table(Events::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Sessions::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(State::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Tasks::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Messages::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Groups::Table).if_exists().to_owned())
            .await?;
        Ok(())
    }
}

async fn create_groups(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(Groups::Table)
                .if_not_exists()
                .col(ColumnDef::new(Groups::Id).text().not_null().primary_key())
                .col(ColumnDef::new(Groups::DisplayName).text().not_null())
                .col(ColumnDef::new(Groups::ConfigJson).text().not_null())
                .col(
                    ColumnDef::new(Groups::Active)
                        .boolean()
                        .not_null()
                        .default(true),
                )
                .col(
                    ColumnDef::new(Groups::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(Groups::UpdatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .to_owned(),
        )
        .await
}

async fn create_messages(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(Messages::Table)
                .if_not_exists()
                .col(ColumnDef::new(Messages::Id).text().not_null().primary_key())
                .col(ColumnDef::new(Messages::GroupId).text().not_null())
                .col(ColumnDef::new(Messages::ChannelId).text().not_null())
                .col(ColumnDef::new(Messages::Sender).text().not_null())
                .col(ColumnDef::new(Messages::Content).text().not_null())
                .col(
                    ColumnDef::new(Messages::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .to_owned(),
        )
        .await?;

    manager
        .create_index(
            Index::create()
                .name("messages_group_cursor_idx")
                .table(Messages::Table)
                .col(Messages::GroupId)
                .col(Messages::CreatedAt)
                .col(Messages::Id)
                .to_owned(),
        )
        .await
}

async fn create_tasks(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(Tasks::Table)
                .if_not_exists()
                .col(ColumnDef::new(Tasks::Id).text().not_null().primary_key())
                .col(ColumnDef::new(Tasks::GroupId).text().not_null())
                .col(ColumnDef::new(Tasks::Prompt).text().not_null())
                .col(ColumnDef::new(Tasks::ScheduleKind).text().not_null())
                .col(ColumnDef::new(Tasks::ScheduleValue).text().not_null())
                .col(ColumnDef::new(Tasks::Status).text().not_null())
                .col(ColumnDef::new(Tasks::NextRun).timestamp_with_time_zone())
                .col(ColumnDef::new(Tasks::LastResult).text())
                .col(
                    ColumnDef::new(Tasks::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(Tasks::UpdatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .to_owned(),
        )
        .await?;

    manager
        .create_index(
            Index::create()
                .name("tasks_due_idx")
                .table(Tasks::Table)
                .col(Tasks::Status)
                .col(Tasks::NextRun)
                .to_owned(),
        )
        .await
}

async fn create_state(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(State::Table)
                .if_not_exists()
                .col(ColumnDef::new(State::Key).text().not_null().primary_key())
                .col(ColumnDef::new(State::Value).text().not_null())
                .col(
                    ColumnDef::new(State::UpdatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .to_owned(),
        )
        .await
}

async fn create_sessions(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(Sessions::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(Sessions::GroupId)
                        .text()
                        .not_null()
                        .primary_key(),
                )
                .col(ColumnDef::new(Sessions::SessionId).text().not_null())
                .col(
                    ColumnDef::new(Sessions::UpdatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .to_owned(),
        )
        .await
}

async fn create_events(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(Events::Table)
                .if_not_exists()
                .col(ColumnDef::new(Events::Id).text().not_null().primary_key())
                .col(ColumnDef::new(Events::Kind).text().not_null())
                .col(ColumnDef::new(Events::GroupId).text())
                .col(ColumnDef::new(Events::Payload).text().not_null())
                .col(
                    ColumnDef::new(Events::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .to_owned(),
        )
        .await?;

    manager
        .create_index(
            Index::create()
                .name("events_created_idx")
                .table(Events::Table)
                .col(Events::CreatedAt)
                .to_owned(),
        )
        .await?;

    manager
        .create_index(
            Index::create()
                .name("events_kind_idx")
                .table(Events::Table)
                .col(Events::Kind)
                .col(Events::CreatedAt)
                .to_owned(),
        )
        .await?;

    manager
        .create_index(
            Index::create()
                .name("events_group_idx")
                .table(Events::Table)
                .col(Events::GroupId)
                .col(Events::CreatedAt)
                .to_owned(),
        )
        .await
}

#[derive(DeriveIden)]
enum Groups {
    Table,
    Id,
    DisplayName,
    ConfigJson,
    Active,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Messages {
    Table,
    Id,
    GroupId,
    ChannelId,
    Sender,
    Content,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Tasks {
    Table,
    Id,
    GroupId,
    Prompt,
    ScheduleKind,
    ScheduleValue,
    Status,
    NextRun,
    LastResult,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum State {
    Table,
    Key,
    Value,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Sessions {
    Table,
    GroupId,
    SessionId,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Events {
    Table,
    Id,
    Kind,
    GroupId,
    Payload,
    CreatedAt,
}
