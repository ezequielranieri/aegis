//! Health check handler: `grpc.health.v1.Health` Check/Watch (REQ-707).
//!
//! Returns SERVING status for all services when the runtime is healthy.

use tonic::{Request, Response, Status};

use crate::proto::aegis::v1::health_check_response::ServingStatus;
use crate::proto::aegis::v1::health_server::Health;
use crate::proto::aegis::v1::{HealthCheckRequest, HealthCheckResponse};

pub struct HealthService;

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl Health for HealthService {
    async fn check(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        let service = request.get_ref().service.as_str();
        tracing::debug!(service = service, "health check requested");

        let status = if service.is_empty() || service == "aegis.v1.AegisRuntime" {
            ServingStatus::Serving
        } else {
            ServingStatus::ServiceUnknown
        };

        Ok(Response::new(HealthCheckResponse {
            status: status.into(),
        }))
    }

    type WatchStream = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<HealthCheckResponse, Status>> + Send + 'static>,
    >;

    async fn watch(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let service = request.get_ref().service.as_str().to_string();
        tracing::debug!(service = %service, "health watch requested");

        let status = if service.is_empty() || service == "aegis.v1.AegisRuntime" {
            ServingStatus::Serving
        } else {
            ServingStatus::ServiceUnknown
        };

        let response = HealthCheckResponse {
            status: status.into(),
        };

        let stream = tokio_stream::iter(vec![Ok(response)]);
        Ok(Response::new(Box::pin(stream)))
    }
}
