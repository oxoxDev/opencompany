use super::*;

#[test]
fn identity_is_opencompany() {
    assert_eq!(PRODUCT_IDENTITY, "opencompany");
}

#[test]
fn header_pairs_the_sdk_name_key_with_the_identity() {
    assert_eq!(product_identity_header(), ("x-sdk-name", "opencompany"));
    assert_eq!(
        product_identity_header(),
        (PRODUCT_IDENTITY_HEADER, PRODUCT_IDENTITY)
    );
}

/// The whole point of this module existing separately from
/// `openhuman_tinyhumans::backend::product`: the two crates' header name AND the
/// fact that this crate's identity is NOT the embedded core's default
/// must never be able to drift apart silently. If either assertion here
/// ever fails, the embedded core changed its header contract (or its
/// default happened to become "opencompany") without this crate noticing
/// — exactly the kind of mismatch that would make the backend attribute
/// OpenCompany traffic to the wrong product, or to no product at all.
#[cfg(feature = "openhuman")]
#[test]
fn stays_in_sync_with_the_embedded_core_and_diverges_from_its_default() {
    assert_eq!(
        PRODUCT_IDENTITY_HEADER,
        openhuman_tinyhumans::backend::PRODUCT_IDENTITY_HEADER,
        "this crate's header name must match the embedded core's exactly"
    );
    assert_ne!(
        PRODUCT_IDENTITY,
        openhuman_tinyhumans::backend::DEFAULT_PRODUCT_IDENTITY,
        "opencompany's identity must not silently become the openhuman default"
    );
}

/// `feedback::tinyhumans::PRODUCT` used to carry its own `"opencompany"`
/// literal (AC #2 of issue #376: the value must be set once, not
/// duplicated per call site). Pinning the equality here means a future
/// edit that re-introduces a second literal there fails this test instead
/// of silently reintroducing the duplication.
#[test]
fn feedback_product_const_stays_wired_to_this_single_source_of_truth() {
    assert_eq!(crate::feedback::tinyhumans::PRODUCT, PRODUCT_IDENTITY);
}
