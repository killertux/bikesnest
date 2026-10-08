//! Application-layer tests for the readiness use case (fake probes).
use async_trait::async_trait;
use bikesnest_application::{CheckReadiness, DatabaseProbe, ProbeError, Readiness};

struct FakeProbe(ProbeError);
struct OkProbe;

#[async_trait]
impl DatabaseProbe for FakeProbe {
    async fn ping(&self) -> Result<(), ProbeError> {
        Err(self.0.clone_probe())
    }
}
#[async_trait]
impl DatabaseProbe for OkProbe {
    async fn ping(&self) -> Result<(), ProbeError> {
        Ok(())
    }
}

// Small helper: ProbeError is not Clone, so map by variant.
trait CloneProbe {
    fn clone_probe(&self) -> ProbeError;
}
impl CloneProbe for ProbeError {
    fn clone_probe(&self) -> ProbeError {
        match self {
            ProbeError::Unavailable => ProbeError::Unavailable,
            ProbeError::Unexpected => ProbeError::Unexpected,
        }
    }
}

#[tokio::test]
async fn ready_when_probe_ok() {
    let uc = CheckReadiness::new(OkProbe);
    assert_eq!(uc.execute().await, Readiness::Ready);
}

#[tokio::test]
async fn dependency_down_is_distinct_from_app_error() {
    let down = CheckReadiness::new(FakeProbe(ProbeError::Unavailable));
    assert_eq!(down.execute().await, Readiness::DependencyDown);

    let broken = CheckReadiness::new(FakeProbe(ProbeError::Unexpected));
    assert_eq!(broken.execute().await, Readiness::AppError);
}

#[tokio::test]
async fn a_boxed_probe_backs_the_default_readiness_type() {
    // The web layer holds `CheckReadiness` (boxed probe) so it never names the
    // concrete adapter.
    let uc: CheckReadiness = CheckReadiness::new(Box::new(OkProbe));
    assert_eq!(uc.execute().await, Readiness::Ready);
}
