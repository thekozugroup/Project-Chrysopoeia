//! `/api/overview`.

use axum::Json;
use axum::extract::State;
use szalinski_core::Overview;

use crate::db;
use crate::db::stats::Bucket;
use crate::error::ApiResult;
use crate::services::dispatcher;
use crate::state::AppState;

/// Days of savings history in the overview.
pub const HISTORY_DAYS: u32 = 30;

/// `GET /api/overview`
pub async fn get(State(state): State<AppState>) -> ApiResult<Json<Overview>> {
    let pool = state.db.pool();
    Ok(Json(Overview {
        totals: db::stats::totals(pool).await?,
        video_codecs: db::stats::buckets(pool, Bucket::VideoCodec).await?,
        audio_codecs: db::stats::buckets(pool, Bucket::AudioCodec).await?,
        resolutions: db::stats::buckets(pool, Bucket::Resolution).await?,
        savings_history: db::stats::savings_history(pool, HISTORY_DAYS).await?,
        projected_savings_bytes: db::stats::projected_savings(pool).await?,
        queue: dispatcher::queue_state(&state).await?,
    }))
}
