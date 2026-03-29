//! Recommendation engine with ROI-based optimization suggestions.
//!
//! Analyzes bottlenecks and generates actionable recommendations with
//! severity-based priority and ROI scoring.

use crate::types::{BottleneckFinding, Recommendation};

/// Generate recommendations from detected bottlenecks
pub fn generate_recommendations(bottlenecks: &[BottleneckFinding]) -> Vec<Recommendation> {
    let mut recommendations = Vec::new();

    for bottleneck in bottlenecks {
        match bottleneck.bottleneck_type.as_str() {
            "StateRootComputation" => {
                recommendations.extend(recommendations_for_state_root(bottleneck));
            }
            "ClientSigning" => {
                recommendations.extend(recommendations_for_client_signing(bottleneck));
            }
            "RPCLatency" => {
                recommendations.extend(recommendations_for_rpc_latency(bottleneck));
            }
            "ConfirmationLag" => {
                recommendations.extend(recommendations_for_confirmation_lag(bottleneck));
            }
            "MemoryPressure" => {
                recommendations.extend(recommendations_for_memory_pressure(bottleneck));
            }
            "NetworkCongestion" => {
                recommendations.extend(recommendations_for_network_congestion(bottleneck));
            }
            _ => {}
        }
    }

    // Sort by ROI descending (highest value first)
    recommendations.sort_by(|a, b| {
        b.roi_score
            .partial_cmp(&a.roi_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    recommendations
}

fn recommendations_for_state_root(_bottleneck: &BottleneckFinding) -> Vec<Recommendation> {
    vec![
        Recommendation {
            priority: "Critical".to_string(),
            title: "Implement Verkle Tree Commitment Scheme".to_string(),
            description: "Replace Merkle tree state commitments with Verkle (Vector Commitment) trees for faster state root computation.".to_string(),
            estimated_tps_improvement_pct: 25.0,
            effort_level: "High".to_string(),
            roi_score: (25.0 / 3.0), // impact / effort (High=3, Medium=2, Low=1)
            implementation_hints: vec![
                "Investigate Ipa commitment library".to_string(),
                "Add Verkle tree data structure".to_string(),
                "Implement lazy evaluation and batching".to_string(),
            ],
        },
        Recommendation {
            priority: "High".to_string(),
            title: "Parallelize State Root Computation".to_string(),
            description: "Use SIMD or multithreading to compute state root updates in parallel with transaction validation.".to_string(),
            estimated_tps_improvement_pct: 12.0,
            effort_level: "Medium".to_string(),
            roi_score: (12.0 / 2.0),
            implementation_hints: vec![
                "Profile state root computation path".to_string(),
                "Identify parallelizable subtasks".to_string(),
                "Use rayon for parallel iteration".to_string(),
            ],
        },
        Recommendation {
            priority: "Medium".to_string(),
            title: "Cache State Root Intermediate Values".to_string(),
            description: "Cache merkle subtree hashes to avoid recomputation across blocks.".to_string(),
            estimated_tps_improvement_pct: 5.0,
            effort_level: "Low".to_string(),
            roi_score: (5.0 / 1.0),
            implementation_hints: vec![
                "Design cache eviction policy".to_string(),
                "Profile cache hit rates".to_string(),
                "Monitor memory overhead".to_string(),
            ],
        },
    ]
}

fn recommendations_for_client_signing(_bottleneck: &BottleneckFinding) -> Vec<Recommendation> {
    vec![
        Recommendation {
            priority: "Critical".to_string(),
            title: "Implement Batch Signature Aggregation".to_string(),
            description: "Use BLS or Schnorr batch signatures to aggregate multiple client signatures into one.".to_string(),
            estimated_tps_improvement_pct: 30.0,
            effort_level: "High".to_string(),
            roi_score: (30.0 / 3.0),
            implementation_hints: vec![
                "Select batch signature scheme (BLS/Schnorr)".to_string(),
                "Implement signature aggregation".to_string(),
                "Add verification logic".to_string(),
            ],
        },
        Recommendation {
            priority: "High".to_string(),
            title: "Use SIMD-Accelerated Signing".to_string(),
            description: "Leverage SIMD instructions for parallel elliptic curve operations in signing.".to_string(),
            estimated_tps_improvement_pct: 15.0,
            effort_level: "Medium".to_string(),
            roi_score: (15.0 / 2.0),
            implementation_hints: vec![
                "Profile signing bottleneck with perf".to_string(),
                "Evaluate libsecp256k1 SIMD fork".to_string(),
                "Benchmark Rust crypto crates".to_string(),
            ],
        },
        Recommendation {
            priority: "Medium".to_string(),
            title: "Pre-Compute Signature Nonces".to_string(),
            description: "Generate random nonces offline and cache them to reduce online signing latency.".to_string(),
            estimated_tps_improvement_pct: 8.0,
            effort_level: "Low".to_string(),
            roi_score: (8.0 / 1.0),
            implementation_hints: vec![
                "Add nonce pool manager".to_string(),
                "Implement background nonce generation".to_string(),
                "Monitor pool depletion".to_string(),
            ],
        },
    ]
}

fn recommendations_for_rpc_latency(_bottleneck: &BottleneckFinding) -> Vec<Recommendation> {
    vec![
        Recommendation {
            priority: "Critical".to_string(),
            title: "Implement HTTP/2 Multiplexing".to_string(),
            description: "Switch from HTTP/1.1 to HTTP/2 to multiplex multiple RPC requests over single connection.".to_string(),
            estimated_tps_improvement_pct: 20.0,
            effort_level: "Medium".to_string(),
            roi_score: (20.0 / 2.0),
            implementation_hints: vec![
                "Upgrade reqwest to HTTP/2 support".to_string(),
                "Enable connection pooling".to_string(),
                "Benchmark with h2 protocol".to_string(),
            ],
        },
        Recommendation {
            priority: "High".to_string(),
            title: "Use Local RPC Endpoint".to_string(),
            description: "Deploy RPC server on same machine or network with <1ms latency, eliminating network jitter.".to_string(),
            estimated_tps_improvement_pct: 15.0,
            effort_level: "Low".to_string(),
            roi_score: (15.0 / 1.0),
            implementation_hints: vec![
                "Deploy RPC on localhost:8545".to_string(),
                "Configure firewall rules".to_string(),
                "Measure latency with ping/iperf".to_string(),
            ],
        },
        Recommendation {
            priority: "Medium".to_string(),
            title: "Batch RPC Requests".to_string(),
            description: "Group multiple RPC calls into single batch request to reduce round-trip overhead.".to_string(),
            estimated_tps_improvement_pct: 8.0,
            effort_level: "Medium".to_string(),
            roi_score: (8.0 / 2.0),
            implementation_hints: vec![
                "Implement JSON-RPC batch API client".to_string(),
                "Batch transaction submission confirmations".to_string(),
                "Profile batch size optimization".to_string(),
            ],
        },
    ]
}

fn recommendations_for_confirmation_lag(_bottleneck: &BottleneckFinding) -> Vec<Recommendation> {
    vec![
        Recommendation {
            priority: "Critical".to_string(),
            title: "Increase Block Production Rate".to_string(),
            description: "Reduce block time from X seconds to Y seconds to process pending transactions faster.".to_string(),
            estimated_tps_improvement_pct: 25.0,
            effort_level: "High".to_string(),
            roi_score: (25.0 / 3.0),
            implementation_hints: vec![
                "Lower block.time in consensus config".to_string(),
                "Re-tune validator hardware".to_string(),
                "Monitor uncle/orphan block rate".to_string(),
            ],
        },
        Recommendation {
            priority: "High".to_string(),
            title: "Parallelize Block Validation".to_string(),
            description: "Validate transactions in parallel instead of sequentially to speed up block processing.".to_string(),
            estimated_tps_improvement_pct: 18.0,
            effort_level: "High".to_string(),
            roi_score: (18.0 / 3.0),
            implementation_hints: vec![
                "Profile transaction validation".to_string(),
                "Identify parallelizable operations".to_string(),
                "Use rayon thread pool".to_string(),
            ],
        },
        Recommendation {
            priority: "Medium".to_string(),
            title: "Use MEV-Aware Block Building".to_string(),
            description: "Optimize transaction ordering to maximize block utilization and transaction throughput.".to_string(),
            estimated_tps_improvement_pct: 10.0,
            effort_level: "Medium".to_string(),
            roi_score: (10.0 / 2.0),
            implementation_hints: vec![
                "Implement MEV-aware ordering".to_string(),
                "Profile block packing efficiency".to_string(),
                "Test with various fee structures".to_string(),
            ],
        },
    ]
}

fn recommendations_for_memory_pressure(_bottleneck: &BottleneckFinding) -> Vec<Recommendation> {
    vec![
        Recommendation {
            priority: "Critical".to_string(),
            title: "Implement Smart Contract State Pruning".to_string(),
            description: "Remove old historical state and archived data to reduce memory footprint.".to_string(),
            estimated_tps_improvement_pct: 20.0,
            effort_level: "High".to_string(),
            roi_score: (20.0 / 3.0),
            implementation_hints: vec![
                "Implement state archival system".to_string(),
                "Add pruning strategy (age-based)".to_string(),
                "Monitor cold storage retention".to_string(),
            ],
        },
        Recommendation {
            priority: "High".to_string(),
            title: "Use Efficient Hash Map Structures".to_string(),
            description: "Replace standard HashMaps with memory-efficient alternatives (e.g., DashMap, parking_lot).".to_string(),
            estimated_tps_improvement_pct: 12.0,
            effort_level: "Medium".to_string(),
            roi_score: (12.0 / 2.0),
            implementation_hints: vec![
                "Benchmark HashMap alternatives".to_string(),
                "Profile memory allocations".to_string(),
                "Use jemalloc for better packing".to_string(),
            ],
        },
        Recommendation {
            priority: "Medium".to_string(),
            title: "Compress Transaction Cache".to_string(),
            description: "Use compression algorithms on cached transactions to reduce memory by 40-50%.".to_string(),
            estimated_tps_improvement_pct: 8.0,
            effort_level: "Low".to_string(),
            roi_score: (8.0 / 1.0),
            implementation_hints: vec![
                "Evaluate compression libraries (zstd, snappy)".to_string(),
                "Measure compression ratio and latency".to_string(),
                "Profile CPU impact of decompression".to_string(),
            ],
        },
    ]
}

fn recommendations_for_network_congestion(_bottleneck: &BottleneckFinding) -> Vec<Recommendation> {
    vec![
        Recommendation {
            priority: "Critical".to_string(),
            title: "Implement Sparse Block Trees".to_string(),
            description: "Use compact merkle proofs and partial block transmission to reduce block propagation size.".to_string(),
            estimated_tps_improvement_pct: 22.0,
            effort_level: "High".to_string(),
            roi_score: (22.0 / 3.0),
            implementation_hints: vec![
                "Design sparse merkle tree structure".to_string(),
                "Implement partial block requests".to_string(),
                "Add sender-receiver state reconciliation".to_string(),
            ],
        },
        Recommendation {
            priority: "High".to_string(),
            title: "Optimize Block Serialization".to_string(),
            description: "Use binary serialization (bincode) instead of JSON to reduce block size by 60%.".to_string(),
            estimated_tps_improvement_pct: 15.0,
            effort_level: "Medium".to_string(),
            roi_score: (15.0 / 2.0),
            implementation_hints: vec![
                "Replace serde_json with bincode".to_string(),
                "Measure serialization size reduction".to_string(),
                "Benchmark CPU overhead".to_string(),
            ],
        },
        Recommendation {
            priority: "Medium".to_string(),
            title: "Use Gossip Protocol Optimization".to_string(),
            description: "Fine-tune gossip parameters (fanout, timeout) for network topology and latency.".to_string(),
            estimated_tps_improvement_pct: 8.0,
            effort_level: "Low".to_string(),
            roi_score: (8.0 / 1.0),
            implementation_hints: vec![
                "Measure network topology".to_string(),
                "Tune gossip fanout (default 8)".to_string(),
                "Profile with varying message sizes".to_string(),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_bottleneck(bottleneck_type: &str, severity: f32) -> BottleneckFinding {
        BottleneckFinding {
            bottleneck_type: bottleneck_type.to_string(),
            severity,
            pct_of_total: 30.0,
            details: "Test bottleneck".to_string(),
            stack_attribution: HashMap::new(),
        }
    }

    #[test]
    fn test_generate_recommendations_state_root() {
        let bottlenecks = vec![make_bottleneck("StateRootComputation", 0.8)];
        let recs = generate_recommendations(&bottlenecks);
        assert!(!recs.is_empty());
        assert!(recs.iter().any(|r| r.title.contains("Verkle")));
    }

    #[test]
    fn test_generate_recommendations_client_signing() {
        let bottlenecks = vec![make_bottleneck("ClientSigning", 0.7)];
        let recs = generate_recommendations(&bottlenecks);
        assert!(!recs.is_empty());
        assert!(recs.iter().any(|r| r.title.contains("Batch")));
    }

    #[test]
    fn test_all_bottleneck_types_handled() {
        let bottleneck_types = vec![
            "StateRootComputation",
            "ClientSigning",
            "RPCLatency",
            "ConfirmationLag",
            "MemoryPressure",
            "NetworkCongestion",
        ];

        for btype in bottleneck_types {
            let bottlenecks = vec![make_bottleneck(btype, 0.5)];
            let recs = generate_recommendations(&bottlenecks);
            assert!(!recs.is_empty(), "No recommendations for {}", btype);
        }
    }

    #[test]
    fn test_recommendations_sorted_by_roi() {
        let bottlenecks = vec![make_bottleneck("StateRootComputation", 0.8)];
        let recs = generate_recommendations(&bottlenecks);

        if recs.len() > 1 {
            for i in 0..recs.len() - 1 {
                assert!(
                    recs[i].roi_score >= recs[i + 1].roi_score,
                    "Recommendations not sorted by ROI"
                );
            }
        }
    }

    #[test]
    fn test_recommendation_fields_present() {
        let bottlenecks = vec![make_bottleneck("ClientSigning", 0.6)];
        let recs = generate_recommendations(&bottlenecks);

        for rec in recs {
            assert!(!rec.priority.is_empty());
            assert!(!rec.title.is_empty());
            assert!(!rec.description.is_empty());
            assert!(rec.estimated_tps_improvement_pct > 0.0);
            assert!(!rec.effort_level.is_empty());
            assert!(rec.roi_score > 0.0);
            assert!(!rec.implementation_hints.is_empty());
        }
    }

    #[test]
    fn test_multiple_bottlenecks_generate_multiple_recommendations() {
        let bottlenecks = vec![
            make_bottleneck("StateRootComputation", 0.8),
            make_bottleneck("ClientSigning", 0.7),
        ];
        let recs = generate_recommendations(&bottlenecks);
        assert!(recs.len() >= 3); // At least 3 recommendations per bottleneck
    }

    #[test]
    fn test_empty_bottlenecks_returns_empty() {
        let recs = generate_recommendations(&[]);
        assert!(recs.is_empty());
    }

    #[test]
    fn test_unknown_bottleneck_type_ignored() {
        let bottlenecks = vec![make_bottleneck("UnknownType", 0.9)];
        let recs = generate_recommendations(&bottlenecks);
        assert!(recs.is_empty());
    }

    #[test]
    fn test_rpc_latency_recommendations() {
        let bottlenecks = vec![make_bottleneck("RPCLatency", 0.6)];
        let recs = generate_recommendations(&bottlenecks);
        assert!(!recs.is_empty());
        assert!(recs.iter().any(|r| r.title.contains("HTTP/2")));
    }

    #[test]
    fn test_confirmation_lag_recommendations() {
        let bottlenecks = vec![make_bottleneck("ConfirmationLag", 0.5)];
        let recs = generate_recommendations(&bottlenecks);
        assert!(!recs.is_empty());
        assert!(recs.iter().any(|r| r.title.contains("Block Production")));
    }

    #[test]
    fn test_memory_pressure_recommendations() {
        let bottlenecks = vec![make_bottleneck("MemoryPressure", 0.7)];
        let recs = generate_recommendations(&bottlenecks);
        assert!(!recs.is_empty());
        assert!(recs.iter().any(|r| r.title.contains("Pruning")));
    }

    #[test]
    fn test_network_congestion_recommendations() {
        let bottlenecks = vec![make_bottleneck("NetworkCongestion", 0.6)];
        let recs = generate_recommendations(&bottlenecks);
        assert!(!recs.is_empty());
        assert!(recs.iter().any(|r| r.title.contains("Sparse")));
    }

    #[test]
    fn test_all_recommendations_have_three_hints() {
        let bottleneck_types = vec![
            "StateRootComputation",
            "ClientSigning",
            "RPCLatency",
            "ConfirmationLag",
            "MemoryPressure",
            "NetworkCongestion",
        ];

        for btype in bottleneck_types {
            let bottlenecks = vec![make_bottleneck(btype, 0.5)];
            let recs = generate_recommendations(&bottlenecks);
            for rec in &recs {
                assert_eq!(
                    rec.implementation_hints.len(),
                    3,
                    "Recommendation '{}' for {} should have 3 hints",
                    rec.title,
                    btype
                );
            }
        }
    }

    #[test]
    fn test_recommendations_roi_consistency() {
        // Each bottleneck type produces 3 recs: Critical/High, High/Medium, Medium/Low
        // ROI = improvement / effort_number, so order should be consistent
        let bottlenecks = vec![make_bottleneck("RPCLatency", 0.6)];
        let recs = generate_recommendations(&bottlenecks);
        assert_eq!(recs.len(), 3);
        // Sorted by ROI descending — highest ROI first
        for i in 0..recs.len() - 1 {
            assert!(
                recs[i].roi_score >= recs[i + 1].roi_score,
                "ROI not sorted: {} >= {}",
                recs[i].roi_score,
                recs[i + 1].roi_score
            );
        }
    }
}
