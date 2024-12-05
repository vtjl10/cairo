use std::ops::Deref;

use cairo_lang_debug::DebugWithDb;
use cairo_lang_semantic::db::PluginSuiteInput;
use cairo_lang_semantic::inline_macros::get_default_plugin_suite;
use cairo_lang_semantic::test_utils::TestFunction;
use cairo_lang_test_utils::parse_test_file::TestRunnerResult;
use cairo_lang_utils::ordered_hash_map::OrderedHashMap;

use crate::db::LoweringGroup;
use crate::fmt::LoweredFormatter;
use crate::ids::ConcreteFunctionWithBodyId;
use crate::inline::apply_inlining;
use crate::optimizations::remappings::optimize_remappings;
use crate::optimizations::reorder_statements::reorder_statements;
use crate::optimizations::return_optimization::return_optimization;
use crate::panic::lower_panics;
use crate::reorganize_blocks::reorganize_blocks;
use crate::test_utils::LoweringDatabaseForTesting;

cairo_lang_test_utils::test_file_test!(
    return_optimizer,
    "src/optimizations/test_data",
    {
        return_optimization: "return_optimization"
    },
    test_return_optimizer
);

fn test_return_optimizer(
    inputs: &OrderedHashMap<String, String>,
    _args: &OrderedHashMap<String, String>,
) -> TestRunnerResult {
    let db = &mut LoweringDatabaseForTesting::default();

    let test_function_builder = TestFunction::builder(
        db,
        inputs["function"].as_str(),
        inputs["function_name"].as_str(),
        inputs["module_code"].as_str(),
        None,
    );

    let crate_id = unsafe { test_function_builder.get_crate_id() };
    db.set_crate_plugins_from_suite(crate_id, get_default_plugin_suite());

    let (test_function, semantic_diagnostics) =
        test_function_builder.build_and_check_for_diagnostics(db).split();

    let function_id =
        ConcreteFunctionWithBodyId::from_semantic(db, test_function.concrete_function_id);

    let mut before =
        db.priv_concrete_function_with_body_lowered_flat(function_id).unwrap().deref().clone();
    let lowering_diagnostics = db.module_lowering_diagnostics(test_function.module_id).unwrap();

    apply_inlining(db, function_id, &mut before).unwrap();
    before = lower_panics(db, function_id, &before).unwrap();
    optimize_remappings(&mut before);
    reorganize_blocks(&mut before);
    reorder_statements(db, &mut before);

    let mut after = before.clone();
    return_optimization(db, &mut after);

    TestRunnerResult::success(OrderedHashMap::from([
        ("semantic_diagnostics".into(), semantic_diagnostics),
        (
            "before".into(),
            format!("{:?}", before.debug(&LoweredFormatter::new(db, &before.variables))),
        ),
        (
            "after".into(),
            format!("{:?}", after.debug(&LoweredFormatter::new(db, &after.variables))),
        ),
        ("lowering_diagnostics".into(), lowering_diagnostics.format(db)),
    ]))
}
