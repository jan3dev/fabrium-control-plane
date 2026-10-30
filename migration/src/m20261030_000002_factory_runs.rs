use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(FactoryRuns::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FactoryRuns::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FactoryRuns::Repo).string_len(200).not_null())
                    .col(ColumnDef::new(FactoryRuns::Issue).big_integer().not_null())
                    .col(ColumnDef::new(FactoryRuns::State).string_len(32).not_null())
                    .col(ColumnDef::new(FactoryRuns::Branch).string_len(200).null())
                    .col(ColumnDef::new(FactoryRuns::Pr).big_integer().null())
                    .col(ColumnDef::new(FactoryRuns::TaskRoom).string_len(255).null())
                    .col(
                        ColumnDef::new(FactoryRuns::ReviewRoom)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(FactoryRuns::CiAttempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(FactoryRuns::ReviewAttempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(FactoryRuns::ReviewHead)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(FactoryRuns::BudgetEndsAt)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(FactoryRuns::CreatedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FactoryRuns::UpdatedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .index(
                        Index::create()
                            .name("idx_factory_runs_repo_issue")
                            .table(FactoryRuns::Table)
                            .col(FactoryRuns::Repo)
                            .col(FactoryRuns::Issue)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(FactoryRuns::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum FactoryRuns {
    Table,
    Id,
    Repo,
    Issue,
    State,
    Branch,
    Pr,
    TaskRoom,
    ReviewRoom,
    CiAttempts,
    ReviewAttempts,
    ReviewHead,
    BudgetEndsAt,
    CreatedAt,
    UpdatedAt,
}
