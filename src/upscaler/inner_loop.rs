//! Pure inner-loop selection for the ML upscaler.
//!
//! DP4A is the default on RDNA 3. Cooperative matrix is kept only when every
//! dependency is present and an on-target benchmark shows a meaningful win.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InnerLoop {
    Dp4a,
    CooperativeMatrix,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InnerLoopSupport {
    pub dot_product: bool,
    pub coopmat: bool,
    pub shader_int8: bool,
    pub storage_8bit: bool,
    pub memory_model: bool,
}

impl InnerLoopSupport {
    pub fn coopmat_ready(self) -> bool {
        self.coopmat && self.shader_int8 && self.storage_8bit && self.memory_model
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BenchmarkResult {
    pub dp4a_ms: f32,
    pub coopmat_ms: f32,
}

impl BenchmarkResult {
    fn coopmat_win(self) -> bool {
        self.dp4a_ms.is_finite()
            && self.coopmat_ms.is_finite()
            && self.dp4a_ms > 0.0
            && self.coopmat_ms > 0.0
            && self.coopmat_ms < self.dp4a_ms * 0.85
    }
}

pub fn choose_inner_loop(
    support: InnerLoopSupport,
    benchmark: Option<BenchmarkResult>,
) -> Option<InnerLoop> {
    if support.dot_product {
        if support.coopmat_ready() && benchmark.is_some_and(BenchmarkResult::coopmat_win) {
            Some(InnerLoop::CooperativeMatrix)
        } else {
            Some(InnerLoop::Dp4a)
        }
    } else if support.coopmat_ready() {
        Some(InnerLoop::CooperativeMatrix)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_support() -> InnerLoopSupport {
        InnerLoopSupport {
            dot_product: true,
            coopmat: true,
            shader_int8: true,
            storage_8bit: true,
            memory_model: true,
        }
    }

    #[test]
    fn choose_inner_loop_selects_dp4a_by_default() {
        assert_eq!(
            choose_inner_loop(full_support(), None),
            Some(InnerLoop::Dp4a)
        );
        assert_eq!(
            choose_inner_loop(
                full_support(),
                Some(BenchmarkResult {
                    dp4a_ms: 1.0,
                    coopmat_ms: 0.95
                })
            ),
            Some(InnerLoop::Dp4a)
        );
    }

    #[test]
    fn choose_inner_loop_keeps_coopmat_only_with_a_meaningful_win() {
        assert_eq!(
            choose_inner_loop(
                full_support(),
                Some(BenchmarkResult {
                    dp4a_ms: 1.0,
                    coopmat_ms: 0.80
                })
            ),
            Some(InnerLoop::CooperativeMatrix)
        );
    }

    #[test]
    fn choose_inner_loop_rejects_missing_coopmat_dependencies() {
        for support in [
            InnerLoopSupport {
                shader_int8: false,
                ..full_support()
            },
            InnerLoopSupport {
                storage_8bit: false,
                ..full_support()
            },
            InnerLoopSupport {
                memory_model: false,
                ..full_support()
            },
            InnerLoopSupport {
                coopmat: false,
                ..full_support()
            },
        ] {
            assert_eq!(choose_inner_loop(support, None), Some(InnerLoop::Dp4a));
        }
    }

    #[test]
    fn choose_inner_loop_uses_coopmat_when_dp4a_is_unavailable() {
        assert_eq!(
            choose_inner_loop(
                InnerLoopSupport {
                    dot_product: false,
                    ..full_support()
                },
                None
            ),
            Some(InnerLoop::CooperativeMatrix)
        );
    }

    #[test]
    fn choose_inner_loop_returns_none_without_any_feature() {
        assert_eq!(choose_inner_loop(InnerLoopSupport::default(), None), None);
    }

    #[test]
    fn invalid_benchmark_does_not_select_coopmat() {
        for benchmark in [
            BenchmarkResult {
                dp4a_ms: 0.0,
                coopmat_ms: 0.1,
            },
            BenchmarkResult {
                dp4a_ms: f32::NAN,
                coopmat_ms: 0.1,
            },
            BenchmarkResult {
                dp4a_ms: 1.0,
                coopmat_ms: -1.0,
            },
        ] {
            assert_eq!(
                choose_inner_loop(full_support(), Some(benchmark)),
                Some(InnerLoop::Dp4a)
            );
        }
    }
}
