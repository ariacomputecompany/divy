use serde::Serialize;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MODEL_SIZE_BUDGET_SCALE_GIB: f64 = 32.0;
const MIN_EXECUTION_TIME_SECS: f64 = 0.001;
const MIN_THROUGHPUT_MULTIPLIER: f64 = 0.5;
const MAX_THROUGHPUT_MULTIPLIER: f64 = 1.5;
const RESOURCE_MULTIPLIER_BASE: f64 = 0.85;
const RESOURCE_MULTIPLIER_SPAN: f64 = 0.30;

#[derive(Debug, Clone)]
pub struct CreditPolicyInput {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_model_bytes: u64,
    pub total_columns: u32,
    pub assignments: Vec<AssignmentCreditInput>,
}

#[derive(Debug, Clone)]
pub struct AssignmentCreditInput {
    pub worker_id: String,
    pub execution_time_ms: u64,
    pub assigned_capacity_units: u32,
    pub shard_column_start: u32,
    pub shard_column_end: u32,
    pub available_memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreditPolicyOutput {
    pub job_credit_budget: f64,
    pub assignments: Vec<AssignmentCreditOutput>,
}

#[derive(Debug, Clone, Copy)]
pub struct ConsumptionQuoteInput {
    pub prompt_tokens: u32,
    pub requested_completion_tokens: u32,
    pub total_model_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct ConsumptionSettlementInput {
    pub prompt_tokens: u32,
    pub actual_completion_tokens: u32,
    pub total_model_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct RealtimeAccountingInput {
    pub prompt_tokens: u32,
    pub frontier_completion_tokens: u32,
    pub accounted_completion_tokens: u32,
    pub prompt_credits_accounted: bool,
    pub total_model_bytes: u64,
    pub total_columns: u32,
    pub assignments: Vec<AssignmentCreditInput>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ConsumptionPolicyOutput {
    pub model_size_factor: f64,
    pub prompt_credits: f64,
    pub completion_credits: f64,
    pub total_credits: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RealtimeAccountingOutput {
    pub prompt_tokens_delta: u32,
    pub completion_tokens_delta: u32,
    pub frontier_completion_tokens: u32,
    pub contribution: CreditPolicyOutput,
    pub consumption: ConsumptionPolicyOutput,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssignmentCreditOutput {
    pub worker_id: String,
    pub credits: f64,
    pub compute_share: f64,
    pub throughput_multiplier: f64,
    pub resource_pressure_multiplier: f64,
    pub normalized_contribution_share: f64,
    pub measured_service_rate: f64,
    pub reference_service_rate: f64,
    pub memory_pressure: f64,
}

pub fn compute_credit_policy(input: CreditPolicyInput) -> CreditPolicyOutput {
    if input.assignments.is_empty() {
        return CreditPolicyOutput {
            job_credit_budget: 0.0,
            assignments: Vec::new(),
        };
    }

    let total_tokens = input
        .prompt_tokens
        .saturating_add(input.completion_tokens)
        .max(1) as f64;
    let model_size_factor =
        (input.total_model_bytes as f64 / GIB / MODEL_SIZE_BUDGET_SCALE_GIB).max(1.0);
    let job_credit_budget = total_tokens * model_size_factor;

    let prepared = input
        .assignments
        .iter()
        .map(|assignment| {
            let shard_columns = assignment
                .shard_column_end
                .saturating_sub(assignment.shard_column_start)
                .max(1);
            let shard_fraction = shard_columns as f64 / input.total_columns.max(1) as f64;
            let compute_units =
                assignment.assigned_capacity_units.max(1) as f64 * shard_fraction.max(f64::EPSILON);
            let execution_time_secs =
                (assignment.execution_time_ms as f64 / 1000.0).max(MIN_EXECUTION_TIME_SECS);
            let service_rate = compute_units / execution_time_secs;
            let estimated_memory_bytes = input.total_model_bytes as f64 * shard_fraction;
            let memory_pressure = if assignment.available_memory_bytes == 0 {
                1.0
            } else {
                (estimated_memory_bytes / assignment.available_memory_bytes as f64).clamp(0.0, 1.0)
            };

            PreparedAssignment {
                worker_id: assignment.worker_id.clone(),
                compute_units,
                service_rate,
                memory_pressure,
            }
        })
        .collect::<Vec<_>>();

    let total_compute_units = prepared
        .iter()
        .map(|assignment| assignment.compute_units)
        .sum::<f64>()
        .max(f64::EPSILON);
    let reference_service_rate = median(
        prepared
            .iter()
            .map(|assignment| assignment.service_rate)
            .collect(),
    )
    .max(f64::EPSILON);

    let adjusted_weights = prepared
        .iter()
        .map(|assignment| {
            let compute_share = assignment.compute_units / total_compute_units;
            let throughput_multiplier = (assignment.service_rate / reference_service_rate)
                .clamp(MIN_THROUGHPUT_MULTIPLIER, MAX_THROUGHPUT_MULTIPLIER);
            let resource_pressure_multiplier =
                RESOURCE_MULTIPLIER_BASE + (RESOURCE_MULTIPLIER_SPAN * assignment.memory_pressure);
            compute_share * throughput_multiplier * resource_pressure_multiplier
        })
        .collect::<Vec<_>>();

    let total_adjusted_weight = adjusted_weights
        .iter()
        .copied()
        .sum::<f64>()
        .max(f64::EPSILON);

    let assignments = prepared
        .into_iter()
        .zip(adjusted_weights)
        .map(|(assignment, adjusted_weight)| {
            let compute_share = assignment.compute_units / total_compute_units;
            let throughput_multiplier = (assignment.service_rate / reference_service_rate)
                .clamp(MIN_THROUGHPUT_MULTIPLIER, MAX_THROUGHPUT_MULTIPLIER);
            let resource_pressure_multiplier =
                RESOURCE_MULTIPLIER_BASE + (RESOURCE_MULTIPLIER_SPAN * assignment.memory_pressure);
            let normalized_contribution_share = adjusted_weight / total_adjusted_weight;
            AssignmentCreditOutput {
                worker_id: assignment.worker_id,
                credits: job_credit_budget * normalized_contribution_share,
                compute_share,
                throughput_multiplier,
                resource_pressure_multiplier,
                normalized_contribution_share,
                measured_service_rate: assignment.service_rate,
                reference_service_rate,
                memory_pressure: assignment.memory_pressure,
            }
        })
        .collect();

    CreditPolicyOutput {
        job_credit_budget,
        assignments,
    }
}

pub fn quote_consumption(input: ConsumptionQuoteInput) -> ConsumptionPolicyOutput {
    compute_consumption(
        input.prompt_tokens,
        input.requested_completion_tokens,
        input.total_model_bytes,
    )
}

pub fn settle_consumption(input: ConsumptionSettlementInput) -> ConsumptionPolicyOutput {
    compute_consumption(
        input.prompt_tokens,
        input.actual_completion_tokens,
        input.total_model_bytes,
    )
}

pub fn compute_consumption_components(
    prompt_tokens: u32,
    completion_tokens: u32,
    total_model_bytes: u64,
) -> ConsumptionPolicyOutput {
    compute_consumption(prompt_tokens, completion_tokens, total_model_bytes)
}

pub fn compute_realtime_accounting_delta(
    input: RealtimeAccountingInput,
) -> Option<RealtimeAccountingOutput> {
    let prompt_tokens_delta = if input.prompt_credits_accounted {
        0
    } else {
        input.prompt_tokens
    };
    let completion_tokens_delta = input
        .frontier_completion_tokens
        .saturating_sub(input.accounted_completion_tokens);

    if prompt_tokens_delta == 0 && completion_tokens_delta == 0 {
        return None;
    }

    let contribution = compute_credit_policy(CreditPolicyInput {
        prompt_tokens: prompt_tokens_delta,
        completion_tokens: completion_tokens_delta,
        total_model_bytes: input.total_model_bytes,
        total_columns: input.total_columns,
        assignments: input.assignments,
    });
    let consumption = compute_consumption_components(
        prompt_tokens_delta,
        completion_tokens_delta,
        input.total_model_bytes,
    );

    Some(RealtimeAccountingOutput {
        prompt_tokens_delta,
        completion_tokens_delta,
        frontier_completion_tokens: input.frontier_completion_tokens,
        contribution,
        consumption,
    })
}

#[derive(Debug, Clone)]
struct PreparedAssignment {
    worker_id: String,
    compute_units: f64,
    service_rate: f64,
    memory_pressure: f64,
}

fn compute_consumption(
    prompt_tokens: u32,
    completion_tokens: u32,
    total_model_bytes: u64,
) -> ConsumptionPolicyOutput {
    let model_size_factor = (total_model_bytes as f64 / GIB / MODEL_SIZE_BUDGET_SCALE_GIB).max(1.0);
    let prompt_credits = prompt_tokens as f64 * model_size_factor;
    let completion_credits = completion_tokens as f64 * model_size_factor;

    ConsumptionPolicyOutput {
        model_size_factor,
        prompt_credits,
        completion_credits,
        total_credits: prompt_credits + completion_credits,
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(left: f64, right: f64) {
        let delta = (left - right).abs();
        assert!(delta < 1e-9, "expected {left} ~= {right}, delta={delta}");
    }

    #[test]
    fn normalizes_credits_to_job_budget() {
        let result = compute_credit_policy(CreditPolicyInput {
            prompt_tokens: 8,
            completion_tokens: 4,
            total_model_bytes: 64 * 1024 * 1024 * 1024,
            total_columns: 8192,
            assignments: vec![
                AssignmentCreditInput {
                    worker_id: "worker-a".into(),
                    execution_time_ms: 500,
                    assigned_capacity_units: 4,
                    shard_column_start: 0,
                    shard_column_end: 4096,
                    available_memory_bytes: 16 * 1024 * 1024 * 1024,
                },
                AssignmentCreditInput {
                    worker_id: "worker-b".into(),
                    execution_time_ms: 500,
                    assigned_capacity_units: 4,
                    shard_column_start: 4096,
                    shard_column_end: 8192,
                    available_memory_bytes: 16 * 1024 * 1024 * 1024,
                },
            ],
        });

        let credited = result
            .assignments
            .iter()
            .map(|item| item.credits)
            .sum::<f64>();
        approx_eq(credited, result.job_credit_budget);
        approx_eq(result.assignments[0].credits, result.assignments[1].credits);
    }

    #[test]
    fn rewards_faster_service_for_same_assigned_work() {
        let result = compute_credit_policy(CreditPolicyInput {
            prompt_tokens: 8,
            completion_tokens: 4,
            total_model_bytes: 64 * 1024 * 1024 * 1024,
            total_columns: 8192,
            assignments: vec![
                AssignmentCreditInput {
                    worker_id: "fast".into(),
                    execution_time_ms: 400,
                    assigned_capacity_units: 4,
                    shard_column_start: 0,
                    shard_column_end: 4096,
                    available_memory_bytes: 16 * 1024 * 1024 * 1024,
                },
                AssignmentCreditInput {
                    worker_id: "slow".into(),
                    execution_time_ms: 1200,
                    assigned_capacity_units: 4,
                    shard_column_start: 4096,
                    shard_column_end: 8192,
                    available_memory_bytes: 16 * 1024 * 1024 * 1024,
                },
            ],
        });

        assert!(result.assignments[0].credits > result.assignments[1].credits);
        assert!(
            result.assignments[0].throughput_multiplier
                > result.assignments[1].throughput_multiplier
        );
    }

    #[test]
    fn rewards_larger_assigned_work_even_when_service_is_equal() {
        let result = compute_credit_policy(CreditPolicyInput {
            prompt_tokens: 8,
            completion_tokens: 4,
            total_model_bytes: 64 * 1024 * 1024 * 1024,
            total_columns: 8192,
            assignments: vec![
                AssignmentCreditInput {
                    worker_id: "large".into(),
                    execution_time_ms: 1000,
                    assigned_capacity_units: 8,
                    shard_column_start: 0,
                    shard_column_end: 6144,
                    available_memory_bytes: 24 * 1024 * 1024 * 1024,
                },
                AssignmentCreditInput {
                    worker_id: "small".into(),
                    execution_time_ms: 1000,
                    assigned_capacity_units: 4,
                    shard_column_start: 6144,
                    shard_column_end: 8192,
                    available_memory_bytes: 24 * 1024 * 1024 * 1024,
                },
            ],
        });

        assert!(result.assignments[0].compute_share > result.assignments[1].compute_share);
        assert!(result.assignments[0].credits > result.assignments[1].credits);
    }

    #[test]
    fn rewards_higher_memory_pressure_when_other_factors_match() {
        let result = compute_credit_policy(CreditPolicyInput {
            prompt_tokens: 8,
            completion_tokens: 4,
            total_model_bytes: 64 * 1024 * 1024 * 1024,
            total_columns: 8192,
            assignments: vec![
                AssignmentCreditInput {
                    worker_id: "tight".into(),
                    execution_time_ms: 800,
                    assigned_capacity_units: 4,
                    shard_column_start: 0,
                    shard_column_end: 4096,
                    available_memory_bytes: 40 * 1024 * 1024 * 1024,
                },
                AssignmentCreditInput {
                    worker_id: "roomy".into(),
                    execution_time_ms: 800,
                    assigned_capacity_units: 4,
                    shard_column_start: 4096,
                    shard_column_end: 8192,
                    available_memory_bytes: 80 * 1024 * 1024 * 1024,
                },
            ],
        });

        assert!(result.assignments[0].memory_pressure > result.assignments[1].memory_pressure);
        assert!(
            result.assignments[0].resource_pressure_multiplier
                > result.assignments[1].resource_pressure_multiplier
        );
        assert!(result.assignments[0].credits > result.assignments[1].credits);
    }

    #[test]
    fn quote_consumption_scales_with_requested_completion_tokens() {
        let result = quote_consumption(ConsumptionQuoteInput {
            prompt_tokens: 8,
            requested_completion_tokens: 4,
            total_model_bytes: 64 * 1024 * 1024 * 1024,
        });

        approx_eq(result.model_size_factor, 2.0);
        approx_eq(result.prompt_credits, 16.0);
        approx_eq(result.completion_credits, 8.0);
        approx_eq(result.total_credits, 24.0);
    }

    #[test]
    fn settle_consumption_uses_actual_completion_tokens() {
        let result = settle_consumption(ConsumptionSettlementInput {
            prompt_tokens: 10,
            actual_completion_tokens: 3,
            total_model_bytes: 32 * 1024 * 1024 * 1024,
        });

        approx_eq(result.model_size_factor, 1.0);
        approx_eq(result.prompt_credits, 10.0);
        approx_eq(result.completion_credits, 3.0);
        approx_eq(result.total_credits, 13.0);
    }

    #[test]
    fn realtime_accounting_emits_prompt_delta_before_completion_frontier_moves() {
        let result = compute_realtime_accounting_delta(RealtimeAccountingInput {
            prompt_tokens: 8,
            frontier_completion_tokens: 0,
            accounted_completion_tokens: 0,
            prompt_credits_accounted: false,
            total_model_bytes: 64 * 1024 * 1024 * 1024,
            total_columns: 8192,
            assignments: vec![
                AssignmentCreditInput {
                    worker_id: "worker-a".into(),
                    execution_time_ms: 500,
                    assigned_capacity_units: 4,
                    shard_column_start: 0,
                    shard_column_end: 4096,
                    available_memory_bytes: 16 * 1024 * 1024 * 1024,
                },
                AssignmentCreditInput {
                    worker_id: "worker-b".into(),
                    execution_time_ms: 500,
                    assigned_capacity_units: 4,
                    shard_column_start: 4096,
                    shard_column_end: 8192,
                    available_memory_bytes: 16 * 1024 * 1024 * 1024,
                },
            ],
        })
        .expect("expected prompt delta");

        assert_eq!(result.prompt_tokens_delta, 8);
        assert_eq!(result.completion_tokens_delta, 0);
        approx_eq(result.consumption.total_credits, 16.0);
        approx_eq(
            result
                .contribution
                .assignments
                .iter()
                .map(|assignment| assignment.credits)
                .sum::<f64>(),
            result.contribution.job_credit_budget,
        );
    }

    #[test]
    fn realtime_accounting_emits_only_new_completion_frontier() {
        let result = compute_realtime_accounting_delta(RealtimeAccountingInput {
            prompt_tokens: 8,
            frontier_completion_tokens: 6,
            accounted_completion_tokens: 3,
            prompt_credits_accounted: true,
            total_model_bytes: 64 * 1024 * 1024 * 1024,
            total_columns: 8192,
            assignments: vec![
                AssignmentCreditInput {
                    worker_id: "worker-a".into(),
                    execution_time_ms: 400,
                    assigned_capacity_units: 4,
                    shard_column_start: 0,
                    shard_column_end: 4096,
                    available_memory_bytes: 16 * 1024 * 1024 * 1024,
                },
                AssignmentCreditInput {
                    worker_id: "worker-b".into(),
                    execution_time_ms: 800,
                    assigned_capacity_units: 4,
                    shard_column_start: 4096,
                    shard_column_end: 8192,
                    available_memory_bytes: 16 * 1024 * 1024 * 1024,
                },
            ],
        })
        .expect("expected completion delta");

        assert_eq!(result.prompt_tokens_delta, 0);
        assert_eq!(result.completion_tokens_delta, 3);
        approx_eq(result.consumption.prompt_credits, 0.0);
        approx_eq(result.consumption.completion_credits, 6.0);
        assert!(
            result.contribution.assignments[0].credits > result.contribution.assignments[1].credits
        );
    }

    #[test]
    fn realtime_accounting_returns_none_when_frontier_is_already_accounted() {
        let result = compute_realtime_accounting_delta(RealtimeAccountingInput {
            prompt_tokens: 8,
            frontier_completion_tokens: 3,
            accounted_completion_tokens: 3,
            prompt_credits_accounted: true,
            total_model_bytes: 64 * 1024 * 1024 * 1024,
            total_columns: 8192,
            assignments: vec![AssignmentCreditInput {
                worker_id: "worker-a".into(),
                execution_time_ms: 400,
                assigned_capacity_units: 4,
                shard_column_start: 0,
                shard_column_end: 8192,
                available_memory_bytes: 32 * 1024 * 1024 * 1024,
            }],
        });

        assert!(result.is_none());
    }
}
