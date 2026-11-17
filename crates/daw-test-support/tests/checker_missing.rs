//! Without the checker installed the helpers refuse to run, rather than
//! passing because nothing counts.

use daw_test_support::no_alloc::assert_no_alloc;

#[test]
#[should_panic(expected = "checker is not installed")]
fn a_binary_without_the_checker_cannot_pass() {
    assert_no_alloc("nothing", || {});
}
