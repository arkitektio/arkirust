//! The shared scripted scenario on the JS event loop.

use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
async fn the_scripted_scenario() {
    let server = wasm_control::run().await.expect("the session ended");
    server.borrow().check();
}

#[wasm_bindgen_test]
async fn the_scripted_scenario_with_a_real_action() {
    let server = wasm_control::run_agent().await.expect("the session ended");
    server.borrow().check();
}
