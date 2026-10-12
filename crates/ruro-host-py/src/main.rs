//! `ruro-host-py`: a ruro host that also runs nodes written in Python.
//!
//! It links the standard Rust nodes and the transform nodes, embeds a Python
//! interpreter and imports Python node modules before handing over to the
//! usual host entry point. Point the daemon at it:
//!
//! ```sh
//! RURO_PY_MODULES=my_nodes ruro daemon --host ruro-host-py
//! ```
//!
//! Node modules come from `RURO_PY_MODULES` (module names or `.py` paths,
//! comma-separated) and from installed distributions that declare a
//! `ruro.nodes` entry point. Set `RURO_PY_RELOAD=1` to re-import a node's
//! module whenever its file changes.

fn main() {
    ruro_tf::link();
    match ruro_py::embed::init() {
        Ok(loaded) => {
            for (module, err) in &loaded.errors {
                eprintln!("ruro-host-py: failed to import {module}:\n{err}");
            }
        }
        Err(e) => eprintln!("ruro-host-py: Python unavailable: {e:#}"),
    }
    if std::env::args().nth(1).as_deref() == Some("schemas") {
        let schemas: Vec<_> = ruro_msg::schema::registered().into_values().collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&schemas).expect("schemas are serializable")
        );
        return;
    }
    ruro_host::main();
}
