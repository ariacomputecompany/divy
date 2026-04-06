# Divy

Divy is a small Rust proof of concept for how a distributed inference network can account for both worker contribution and inference consumption.

It is designed for systems where:

- workers are assigned unequal amounts of work
- devices have different performance envelopes
- slower workers should not earn more simply because they took longer
- total credit movement per job should stay bounded and auditable

## Model

Divy models two linked sides of the same credit system:

- contribution payout across workers
- inference consumption against the submitter

For contribution payout, Divy computes a fixed job budget and then divides that budget across workers using:

- assigned work share
- measured service rate
- memory pressure

At a high level:

`credits = job_budget * normalized(contribution_share * throughput_multiplier * resource_pressure_multiplier)`

The library does not try to infer correctness or trust on its own. It expects callers to pass only verified successful assignments.

## Why

This approach is useful when you want a credit model that:

- rewards useful throughput
- still pays workers that were assigned more real work
- gives a modest premium to hardware under tighter resource pressure
- avoids rewarding stragglers for raw elapsed time
- keeps inference usage bounded by the same unit the workers earn

## Usage

### Contribution payout

```rust
use divy::{compute_credit_policy, AssignmentCreditInput, CreditPolicyInput};

let result = compute_credit_policy(CreditPolicyInput {
    prompt_tokens: 8,
    completion_tokens: 4,
    total_model_bytes: 64 * 1024 * 1024 * 1024,
    total_columns: 8192,
    assignments: vec![
        AssignmentCreditInput {
            worker_id: "fast-metal".into(),
            execution_time_ms: 300,
            assigned_capacity_units: 8,
            shard_column_start: 0,
            shard_column_end: 4096,
            available_memory_bytes: 24 * 1024 * 1024 * 1024,
        },
        AssignmentCreditInput {
            worker_id: "slow-cpu".into(),
            execution_time_ms: 900,
            assigned_capacity_units: 4,
            shard_column_start: 4096,
            shard_column_end: 8192,
            available_memory_bytes: 16 * 1024 * 1024 * 1024,
        },
    ],
});

for assignment in result.assignments {
    println!("{} earned {:.3} credits", assignment.worker_id, assignment.credits);
}
```

### Consumption quote and settlement

```rust
use divy::{
    quote_consumption, settle_consumption, ConsumptionQuoteInput, ConsumptionSettlementInput,
};

let reservation = quote_consumption(ConsumptionQuoteInput {
    prompt_tokens: 128,
    requested_completion_tokens: 256,
    total_model_bytes: 64 * 1024 * 1024 * 1024,
});

let settlement = settle_consumption(ConsumptionSettlementInput {
    prompt_tokens: 128,
    actual_completion_tokens: 180,
    total_model_bytes: 64 * 1024 * 1024 * 1024,
});

assert!(reservation.total_credits >= settlement.total_credits);
```

## Status

Divy is intentionally narrow.

It focuses on the policy core:

- fixed job budgeting
- normalized contribution scoring
- deterministic consumption quote/settlement
- transparent breakdowns per worker and per job

It does not include:

- scheduling
- runtime isolation or sandboxing
- benchmarking orchestration
- fraud detection
- ledger persistence
- networking

## Development

```bash
cargo test
```

## License

MIT
