use axum::{Extension, Json};
use serde_json::{Value, json};

use crate::auth::Client;

pub async fn ping(Extension(client): Extension<Client>) -> Json<Value> {
    Json(json!({ "pong": true, "client": client.name }))
}
