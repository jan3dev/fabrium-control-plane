use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "factory_runs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub repo: String,
    pub issue: i64,
    pub state: String,
    pub branch: Option<String>,
    pub pr: Option<i64>,
    pub task_room: Option<String>,
    pub review_room: Option<String>,
    pub ci_attempts: i32,
    pub review_attempts: i32,
    pub review_head: Option<String>,
    pub budget_ends_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
