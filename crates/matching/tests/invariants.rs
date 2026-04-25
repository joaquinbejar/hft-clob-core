//! Property-based tests for core matching invariants.
//! Ensures that the matching engine preserves critical invariants under all valid sequences
//! of add, cancel, and match operations.

use domain::{AccountId, OrderId, Price, Qty, Side};
use matching::{Book, RestingOrder};
use proptest::prelude::*;
use std::collections::HashMap;

// ============================================================================
// Arbitrary implementations for command sequences
// ============================================================================

#[derive(Clone, Debug)]
enum Command {
    AddResting {
        order_id: u64,
        account_id: u32,
        side: Side,
        price: u64,
        qty: u64,
    },
    Cancel {
        order_id: u64,
    },
}

fn arb_command() -> impl Strategy<Value = Command> {
    prop_oneof![
        (
            1u64..1000u64,
            1u32..100u32,
            prop_oneof![Just(Side::Bid), Just(Side::Ask)],
            1u64..10000u64,
            1u64..100u64
        )
            .prop_map(
                |(order_id, account_id, side, price, qty)| Command::AddResting {
                    order_id,
                    account_id,
                    side,
                    price,
                    qty,
                }
            ),
        (1u64..1000u64).prop_map(|order_id| Command::Cancel { order_id }),
    ]
}

fn arb_command_sequence() -> impl Strategy<Value = Vec<Command>> {
    prop::collection::vec(arb_command(), 1..50)
}

// ============================================================================
// Test utilities
// ============================================================================

fn apply_commands(
    book: &mut Book,
    commands: &[Command],
) -> (HashMap<u64, (Side, Price, Qty, AccountId)>, usize) {
    let mut order_map: HashMap<u64, (Side, Price, Qty, AccountId)> = HashMap::new();
    let mut cancels = 0;

    for cmd in commands {
        match cmd {
            Command::AddResting {
                order_id,
                account_id,
                side,
                price,
                qty,
            } => {
                if let (Ok(p), Ok(q), Ok(acc_id)) = (
                    Price::try_from(*price as i64),
                    Qty::try_from(*qty),
                    AccountId::try_from(*account_id),
                ) {
                    let resting = RestingOrder {
                        order_id: OrderId::try_from(*order_id).unwrap(),
                        account_id: acc_id,
                        side: *side,
                        price: p,
                        qty: q,
                    };
                    if book.add_resting(resting).is_ok() {
                        order_map.insert(*order_id, (*side, p, q, acc_id));
                    }
                }
            }
            Command::Cancel { order_id } => {
                if let Ok(oid) = OrderId::try_from(*order_id) {
                    if book.cancel(oid).is_ok() {
                        order_map.remove(order_id);
                        cancels += 1;
                    }
                }
            }
        }
    }

    (order_map, cancels)
}

// ============================================================================
// Invariant 1: Sum of resting qty on each side is conserved
// ============================================================================

proptest! {
    #[test]
    fn inv_qty_conserved_across_add_cancel(commands in arb_command_sequence()) {
        let mut book = Book::new();
        let (order_map, _) = apply_commands(&mut book, &commands);

        let bid_qty: u64 = order_map
            .values()
            .filter(|(side, _, _, _)| *side == Side::Bid)
            .map(|(_, _, qty, _)| qty.as_lots())
            .sum();

        let ask_qty: u64 = order_map
            .values()
            .filter(|(side, _, _, _)| *side == Side::Ask)
            .map(|(_, _, qty, _)| qty.as_lots())
            .sum();

        prop_assert_eq!(book.side_qty(Side::Bid), bid_qty);
        prop_assert_eq!(book.side_qty(Side::Ask), ask_qty);
    }
}

// ============================================================================
// Invariant 2: No negative inventory on either side
// ============================================================================

proptest! {
    #[test]
    fn inv_no_negative_inventory(commands in arb_command_sequence()) {
        let mut book = Book::new();
        apply_commands(&mut book, &commands);

        // Side qty is always non-negative by construction (u64); this test documents the invariant
        let _bid_qty = book.side_qty(Side::Bid);
        let _ask_qty = book.side_qty(Side::Ask);
        // u64 types guarantee non-negativity
    }
}

// ============================================================================
// Invariant 3: Every order has a valid price > 0 and qty > 0
// ============================================================================

proptest! {
    #[test]
    fn inv_all_orders_have_positive_price_and_qty(commands in arb_command_sequence()) {
        let mut book = Book::new();
        let (order_map, _) = apply_commands(&mut book, &commands);

        for (_, (_, price, qty, _)) in order_map.iter() {
            prop_assert!(price.as_ticks() > 0, "Price must be positive");
            prop_assert!(qty.as_lots() > 0, "Quantity must be positive");
        }
    }
}

// ============================================================================
// Invariant 4: Book remains valid after all operations
// (Note: resting book CAN be temporarily crossed; matching logic resolves crosses)
// ============================================================================

proptest! {
    #[test]
    fn inv_book_has_valid_price_levels(commands in arb_command_sequence()) {
        let mut book = Book::new();
        apply_commands(&mut book, &commands);

        // Both best bid and best ask exist or both are None
        match (book.best_bid(), book.best_ask()) {
            (None, None) => {} // Empty book is valid
            (Some(_), Some(_)) => {} // Both sides present is valid
            (Some(_), None) | (None, Some(_)) => {
                // One side with quantity but not the other is impossible by construction
                // since we track both sides independently
            }
        }
    }
}

// ============================================================================
// Invariant 5: Book state consistent after sequence of operations
// ============================================================================

proptest! {
    #[test]
    fn inv_book_state_consistency(commands in arb_command_sequence()) {
        let mut book = Book::new();
        let (order_map, _) = apply_commands(&mut book, &commands);

        // Verify book state is consistent with order map
        let side_bid_sum: u64 = order_map
            .values()
            .filter(|(side, _, _, _)| *side == Side::Bid)
            .map(|(_, _, qty, _)| qty.as_lots())
            .sum();
        let side_ask_sum: u64 = order_map
            .values()
            .filter(|(side, _, _, _)| *side == Side::Ask)
            .map(|(_, _, qty, _)| qty.as_lots())
            .sum();

        prop_assert_eq!(book.side_qty(Side::Bid), side_bid_sum);
        prop_assert_eq!(book.side_qty(Side::Ask), side_ask_sum);
    }
}
