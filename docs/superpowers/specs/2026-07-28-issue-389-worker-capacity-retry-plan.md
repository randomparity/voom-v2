# Cross-process worker-capacity retry implementation plan

1. Add failing store and control-plane tests for a typed capacity-full
   acquisition outcome and its complete no-mutation durable snapshot.
2. Implement `LeaseAcquireOutcome` while preserving the existing acquisition
   APIs and error code.
3. Add failing executor tests for cross-connection saturation, no dispatch,
   release, timeout, restart, cancellation, and terminal ineligibility.
4. Implement capacity classification and the bounded polling state in the
   executor.
5. Add a CLI process-boundary regression that holds capacity independently,
   proves no request while full, releases it, and observes successful dispatch.
6. Run focused store, control-plane, executor, and CLI tests; run strict lint,
   adversarial review, simplification review, and `just ci`.
