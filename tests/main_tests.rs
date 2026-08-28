// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

#[test]
fn test_shutdown_flag_round_trip() {
    use std::sync::atomic::Ordering;

    // The shutdown flag starts clear and becomes set once requested.
    assert!(!takeout_helper_gphotos::is_shutdown());
    takeout_helper_gphotos::request_shutdown();
    assert!(takeout_helper_gphotos::is_shutdown());

    // Leave the global in its original state for any other test in this binary.
    takeout_helper_gphotos::SHUTDOWN.store(false, Ordering::SeqCst);
}
