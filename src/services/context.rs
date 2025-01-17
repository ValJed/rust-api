#[path = "../sql.rs"]
mod sql;
#[path = "../structs.rs"]
mod structs;
#[path = "../utils.rs"]
mod utils;

use actix_web::{delete, get, post, put, web, HttpResponse, Responder, Result, Scope};
use sqlx::{Pool, Postgres};
use structs::{
    Context, ContextName, ContextRequest, ContextTaskCount, FullContext, FullContextTask,
    IndexQuery,
};
use utils::handle_err;

pub fn get_scope() -> Scope {
    web::scope("/context")
        .service(fetch_all)
        .service(use_or_create)
        .service(update)
        .service(update_by_index)
        .service(delete)
        .service(delete_all)
        .service(clear_active)
}

#[get("")]
pub async fn fetch_all(pool: web::Data<Pool<Postgres>>) -> impl Responder {
    let request = r#"
        SELECT context.*, COUNT(task.id) AS task_count 
        FROM context 
        LEFT JOIN task 
        ON task.context_id = context.id 
        GROUP BY context.id 
        ORDER BY context.id ASC
        "#;

    let contexts_res: Result<Vec<ContextTaskCount>, sqlx::Error> =
        sqlx::query_as(request).fetch_all(pool.get_ref()).await;

    match contexts_res {
        Ok(contexts) => HttpResponse::Ok().json(contexts),
        Err(_) => HttpResponse::InternalServerError().body("Internal Server Error"),
    }
}

#[post("")]
pub async fn use_or_create(
    pool: web::Data<Pool<Postgres>>,
    data: web::Json<ContextRequest>,
) -> impl Responder {
    if data.name.is_empty() {
        return HttpResponse::BadRequest().body("Name is required");
    };

    if data.simple_create.is_some() && data.simple_create.unwrap() {
        let active = data.active.unwrap_or(false);
        let context: Result<Context, sqlx::Error> =
            sqlx::query_as("INSERT INTO context (name, active) VALUES ($1, $2) RETURNING *")
                .bind(data.name.clone())
                .bind(active)
                .fetch_one(pool.get_ref())
                .await;

        if context.is_err() {
            return HttpResponse::InternalServerError().body("Internal Server Error");
        }

        return HttpResponse::Ok().json(context.unwrap());
    }

    let update_req = r#"
        UPDATE context
        SET active = true
        WHERE name = $1
        RETURNING *"#;

    let existing: Result<Option<Context>, sqlx::Error> = sqlx::query_as(update_req)
        .bind(data.name.clone())
        .fetch_optional(pool.get_ref())
        .await;

    if existing.is_err() {
        return HttpResponse::InternalServerError().body("Internal Server Error");
    }

    let _unset_active =
        sqlx::query("UPDATE context SET active = false WHERE active = true AND name != $1")
            .bind(data.name.clone())
            .execute(pool.get_ref())
            .await;

    let ctx = existing.unwrap();

    if ctx.is_none() {
        let context: Result<Context, sqlx::Error> =
            sqlx::query_as("INSERT INTO context (name, active) VALUES ($1, $2) RETURNING *")
                .bind(data.name.clone())
                .bind(true)
                .fetch_one(pool.get_ref())
                .await;

        match context {
            Ok(ctx) => {
                let filled = FullContextTask {
                    id: ctx.id,
                    name: ctx.name,
                    active: ctx.active,
                    tasks: vec![],
                };
                return HttpResponse::Ok().json(filled);
            }
            Err(err) => return handle_err(err),
        }
    }

    let filled_ctx: Result<FullContext, sqlx::Error> = sqlx::query_as(&sql::LIST_TASKS_ACTIVE)
        .fetch_one(pool.get_ref())
        .await;

    match filled_ctx {
        Ok(ctx) => return HttpResponse::Ok().json(ctx),
        Err(err) => return handle_err(err),
    }
}

#[post("/clear")]
pub async fn clear_active(pool: web::Data<Pool<Postgres>>) -> impl Responder {
    let active_ctx: Result<Context, sqlx::Error> =
        sqlx::query_as("SELECT * FROM context WHERE active = true")
            .fetch_one(pool.get_ref())
            .await;

    if active_ctx.is_err() {
        return HttpResponse::NotFound().body("Active context not found");
    }

    let deleted_tasks = sqlx::query("DELETE FROM task WHERE context_id = $1")
        .bind(active_ctx.unwrap().id)
        .execute(pool.get_ref())
        .await;

    if deleted_tasks.is_err() {
        return HttpResponse::InternalServerError().body("Internal Server Error");
    }

    HttpResponse::Ok().body("Context cleared")
}

// Not used right now
#[post("/clear/{id}")]
pub async fn clear(pool: web::Data<Pool<Postgres>>, id: web::Path<i32>) -> impl Responder {
    let deleted_tasks = sqlx::query("DELETE FROM task WHERE context_id = $1")
        .bind(*id)
        .execute(pool.get_ref())
        .await;

    if deleted_tasks.is_err() {
        return HttpResponse::InternalServerError().body("Internal Server Error");
    }

    HttpResponse::Ok().body("Context cleared")
}

#[put("/index/{index}")]
pub async fn update_by_index(
    pool: web::Data<Pool<Postgres>>,
    data: web::Json<ContextName>,
    index: web::Path<i32>,
) -> impl Responder {
    if data.name.is_empty() {
        return HttpResponse::BadRequest().body("Name is required");
    }
    let id = get_context_by_index(&pool, *index, true).await;
    if id.is_none() {
        return HttpResponse::NotFound().body("Context not found");
    }

    let updated: Result<Context, sqlx::Error> =
        sqlx::query_as("UPDATE context SET name = $1 WHERE id = $2 RETURNING *")
            .bind(data.name.clone())
            .bind(id.unwrap())
            .fetch_one(pool.get_ref())
            .await;

    match updated {
        Ok(ctx) => HttpResponse::Ok().json(ctx),
        Err(err) => handle_err(err),
    }
}

#[put("/{id}")]
pub async fn update(
    pool: web::Data<Pool<Postgres>>,
    data: web::Json<Context>,
    id: web::Path<i32>,
) -> impl Responder {
    let existing: Result<Context, sqlx::Error> =
        sqlx::query_as("SELECT * FROM context WHERE id = $1")
            .bind(*id)
            .fetch_one(pool.get_ref())
            .await;

    if existing.is_err() {
        return HttpResponse::NotFound().body("Context not found");
    }

    let existing_ctx: Context = existing.unwrap();

    if data.active && !existing_ctx.active {
        let cleaned = clean_active(&pool, existing_ctx.id, true).await;

        if cleaned.is_err() {
            return HttpResponse::InternalServerError().body("Internal Server Error");
        }
    }

    let updated: Result<Context, sqlx::Error> =
        sqlx::query_as("UPDATE context SET name = $1, active = $2 WHERE id = $3 RETURNING *")
            .bind(data.name.clone())
            .bind(data.active)
            .bind(*id)
            .fetch_one(pool.get_ref())
            .await;

    match updated {
        Ok(ctx) => HttpResponse::Ok().json(ctx),
        Err(err) => handle_err(err),
    }
}

async fn clean_active(
    pool: &web::Data<Pool<Postgres>>,
    id: i32,
    active: bool,
) -> Result<(), sqlx::Error> {
    if active {
        let res =
            sqlx::query("UPDATE context SET active = false WHERE active = true AND NOT id = $1")
                .bind(id)
                .execute(pool.get_ref())
                .await;

        if res.is_err() {
            return Err(res.unwrap_err());
        }

        return Ok(());
    }

    let new_active: Result<Context, sqlx::Error> = sqlx::query_as(
        "SELECT * FROM context WHERE active = false AND NOT id = $1 ORDER BY id LIMIT 1",
    )
    .bind(id)
    .fetch_one(pool.get_ref())
    .await;

    if new_active.is_err() {
        return Err(new_active.unwrap_err());
    }

    let res = sqlx::query("UPDATE context SET active = true WHERE id = $1")
        .bind(new_active.unwrap().id)
        .execute(pool.get_ref())
        .await;

    if res.is_err() {
        return Err(res.unwrap_err());
    }

    return Ok(());
}

#[delete("/{id}")]
pub async fn delete(
    pool: web::Data<Pool<Postgres>>,
    id: web::Path<i32>,
    query: web::Query<IndexQuery>,
) -> impl Responder {
    let by_index = query.index.unwrap_or(false);
    let ctx_id = get_context_by_index(&pool, *id, by_index).await;

    let deleted_tasks = sqlx::query("DELETE FROM task WHERE context_id = $1")
        .bind(ctx_id)
        .execute(pool.get_ref())
        .await;

    if deleted_tasks.is_err() {
        return HttpResponse::InternalServerError().body("Internal Server Error");
    }

    let deleted: Result<Context, sqlx::Error> =
        sqlx::query_as("DELETE FROM context WHERE id = $1 RETURNING * ")
            .bind(ctx_id)
            .fetch_one(pool.get_ref())
            .await;

    match deleted {
        Ok(ctx) => {
            if ctx.active {
                let cleaned = clean_active(&pool, ctx.id, false).await;

                if cleaned.is_err() {
                    return HttpResponse::InternalServerError().body("Internal Server Error");
                }
            }

            return HttpResponse::Ok().json(ctx);
        }
        Err(err) => match err {
            sqlx::Error::RowNotFound => {
                return HttpResponse::NotFound().body("Context not found");
            }
            _ => {
                return HttpResponse::InternalServerError().body("Internal Server Error");
            }
        },
    }
}

#[delete("")]
pub async fn delete_all(pool: web::Data<Pool<Postgres>>) -> impl Responder {
    let deleted: Result<(), sqlx::Error> = sqlx::query_as("DELETE from context")
        .fetch_one(pool.get_ref())
        .await;

    match deleted {
        Ok(_) => {
            return HttpResponse::Ok().body("All contexts deleted");
        }
        Err(err) => match err {
            sqlx::Error::RowNotFound => {
                return HttpResponse::Ok().body("All contexts deleted");
            }
            _ => return handle_err(err),
        },
    }
}

async fn get_context_by_index(
    pool: &web::Data<Pool<Postgres>>,
    index: i32,
    by_index: bool,
) -> Option<i32> {
    if !by_index {
        return Some(index);
    }

    let request = r#"
        SELECT * 
        FROM context 
        ORDER BY context.id ASC;
    "#;
    let contexts: Result<Vec<Context>, sqlx::Error> =
        sqlx::query_as(request).fetch_all(pool.get_ref()).await;

    match contexts {
        Ok(contexts) => match contexts.get(index as usize - 1).cloned() {
            Some(ctx) => Some(ctx.id),
            None => None,
        },
        Err(_) => None,
    }
}
