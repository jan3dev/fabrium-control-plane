use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(GithubDeliveries::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GithubDeliveries::Id)
                            .string_len(64)
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(GithubDeliveries::ReceivedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(GithubDeliveries::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum GithubDeliveries {
    Table,
    Id,
    ReceivedAt,
}
