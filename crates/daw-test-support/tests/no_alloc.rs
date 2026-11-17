//! The checked no-alloc helpers catch what they claim to.

use daw_test_support::no_alloc::{allocations_in, assert_no_alloc};

daw_test_support::install_checker!();

#[test]
fn allocation_free_code_passes_and_returns_its_result() {
    let mut buf = [0u8; 8];
    let sum = assert_no_alloc("summing", || {
        buf.iter_mut().for_each(|b| *b += 1);
        buf.iter().map(|&b| u32::from(b)).sum::<u32>()
    });
    assert_eq!(sum, 8);
}

#[test]
fn allocations_and_frees_are_counted() {
    let n = allocations_in(|| drop(std::hint::black_box(vec![1u8; 16])));
    assert_eq!(n, 2, "one allocation and one free");
    // The count is of this section alone, not a running total.
    let n = allocations_in(|| {});
    assert_eq!(n, 0);
}

#[test]
#[should_panic(expected = "growing a vector called the allocator")]
fn an_allocation_fails_the_assertion() {
    let mut v: Vec<u8> = Vec::new();
    assert_no_alloc("growing a vector", || v.reserve(64));
}
