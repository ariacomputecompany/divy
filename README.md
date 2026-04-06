# Divy

Divy is a small Rust proof of concept for how a distributed inference network can credit worker contribution and burn submitter credits as the same job progresses.

It is designed for systems where:

- workers are assigned unequal amounts of work
- devices have different performance envelopes
- credit movement should stay bounded and auditable
- inference consumption should use the same unit that workers earn

## Model

Divy treats the network as one linked accounting pipeline:

- reserve credits before work starts
- stream contribution credit to workers as the shared frontier advances
- burn submitter credits against that same frontier
- reconcile only the small difference between reserved and actually consumed work

Contribution payout is still based on:

- assigned work share
- measured service rate
- memory pressure

At a high level:

`credits = job_budget * normalized(contribution_share * throughput_multiplier * resource_pressure_multiplier)`

The library does not prove correctness or trust on its own. It expects callers to pass verified assignment and frontier data.

## Why

This approach is useful when you want a credit model that:

- rewards useful throughput
- still pays workers that were assigned more real work
- gives a modest premium to hardware under tighter resource pressure
- avoids rewarding stragglers for raw elapsed time
- keeps inference usage bounded by the same unit the workers earn
- supports realtime wallet-style accounting instead of only end-of-job settlement

## Usage

### Reserve inference upfront

```rust
use divy::{quote_consumption, ConsumptionQuoteInput};

let reservation = quote_consumption(ConsumptionQuoteInput {
    prompt_tokens: 128,
    requested_completion_tokens: 256,
    total_model_bytes: 64 * 1024 * 1024 * 1024,
});

println!("reserve {:.3} credits", reservation.total_credits);
```

### Account for realtime frontier progress

```rust
use divy::{
    compute_realtime_accounting_delta, AssignmentCreditInput, RealtimeAccountingInput,
};

let delta = compute_realtime_accounting_delta(RealtimeAccountingInput {
    prompt_tokens: 128,
    frontier_completion_tokens: 32,
    accounted_completion_tokens: 24,
    prompt_credits_accounted: true,
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
})
.expect("expected new accounting work");

println!("burn {:.3} credits", delta.consumption.total_credits);
for assignment in delta.contribution.assignments {
    println!("{} earned {:.3} credits", assignment.worker_id, assignment.credits);
}
```

### Reconcile actual consumption

```rust
use divy::{settle_consumption, ConsumptionSettlementInput};

let settlement = settle_consumption(ConsumptionSettlementInput {
    prompt_tokens: 128,
    actual_completion_tokens: 180,
    total_model_bytes: 64 * 1024 * 1024 * 1024,
});

println!("actual burn {:.3} credits", settlement.total_credits);
```

## Status

Divy is intentionally narrow.

It focuses on the policy core:

- reservation quote
- normalized contribution scoring
- realtime accounting deltas
- deterministic consumption breakdowns
- transparent per-worker and per-frontier outputs

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
