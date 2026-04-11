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
    // `seq` is the monotonic ingest ordinal used as the cursor key.
    // Assigned by the database on insert, so no caller can produce a
    // sort key smaller than an already-delivered row (closing the
    // backdating hole flagged in the Lane 0.3 audit).
    //
    // `id` is the caller-generated correlation key (UUIDv7), not a
    // sort key. It remains unique so callers can look a message up by
    // its id but cannot use it to affect pagination order.
    manager
        .create_table(
            Table::create()
                .table(Messages::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(Messages::Seq)
                        .big_integer()
                        .not_null()
                        .primary_key()
                        .auto_increment(),
                )
                .col(ColumnDef::new(Messages::Id).text().not_null().unique_key())
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

    // One index supports the cursor query: find rows in a group with
    // seq > cursor.seq, ordered by seq.
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("messages_group_seq_idx")
                .table(Messages::Table)
                .col(Messages::GroupId)
                .col(Messages::Seq)
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

    // Real partial index: only rows with status='active' are indexed,
    // because the due-task query filters on that predicate. SeaQuery
    // emits `WHERE status = 'active'` on both SQLite (≥3.8) and
    // PostgreSQL via `and_where`.
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("tasks_due_idx")
                .table(Tasks::Table)
                .col(Tasks::NextRun)
                .and_where(Expr::col(Tasks::Status).eq("active"))
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
                .if_not_exists()
                .name("events_created_idx")
                .table(Events::Table)
                .col(Events::CreatedAt)
                .to_owned(),
        )
        .await?;

    manager
        .create_index(
            Index::create()
                .if_not_exists()
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
                .if_not_exists()
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
    Seq,
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
