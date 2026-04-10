use anyhow::Result;
use serde::Deserialize;

use super::super::{ToolContext, web};
use super::common::{parse_request, run_node};

#[derive(Default, Deserialize)]
struct WebFetchRequest {
    url: String,
    max_length: Option<usize>,
}

#[derive(Default, Deserialize)]
struct WebSearchRequest {
    query: String,
}

#[derive(Default, Deserialize)]
struct PlanModeRequest {
    active: bool,
}

pub(super) async fn handle(ctx: &mut ToolContext<'_>) -> Result<bool> {
    match ctx.name {
        "web_fetch" => {
            let Some(request) = parse_request::<WebFetchRequest>(ctx) else {
                return Ok(true);
            };
            run_node(
                web::WebFetch {
                    call_id: ctx.call_id.to_string(),
                    url: request.url,
                    max_length: request.max_length,
                },
                ctx,
            )
            .await;
        }
        "web_search" => {
            let Some(request) = parse_request::<WebSearchRequest>(ctx) else {
                return Ok(true);
            };
            run_node(
                web::WebSearch {
                    call_id: ctx.call_id.to_string(),
                    query: request.query,
                },
                ctx,
            )
            .await;
        }
        "plan_mode" => {
            let Some(request) = parse_request::<PlanModeRequest>(ctx) else {
                return Ok(true);
            };
            ctx.store.state.plan_mode = request.active;
            let status = if request.active {
                "Plan mode ON — research and plan only, no execution."
            } else {
                "Plan mode OFF — resuming normal execution."
            };
            ctx.push_result(status);
        }
        _ => return Ok(false),
    }

    Ok(true)
}
