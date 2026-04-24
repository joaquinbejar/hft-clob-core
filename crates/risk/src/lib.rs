//! Risk and operational controls.
//!
//! Pre-trade and in-trade checks: kill switch, per-account max order count,
//! per-account max notional exposure, price band / fat-finger check,
//! self-trade prevention (cancel-both policy), and rejection reason codes.
//!
//! These checks are invoked before matching steps. The kill switch is
//! observed at the engine level so that outstanding orders are not matched
//! or emitted after the kill is engaged.
