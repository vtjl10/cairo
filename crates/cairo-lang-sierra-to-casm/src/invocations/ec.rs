use std::str::FromStr;

use cairo_lang_casm::builder::{CasmBuilder, Var};
use cairo_lang_casm::casm_build_extend;
use cairo_lang_sierra::extensions::ec::EcConcreteLibfunc;
use cairo_lang_sierra::extensions::gas::CostTokenType;
use num_bigint::BigInt;
use starknet_types_core::felt::{Felt as Felt252, NonZeroFelt};

use super::{CompiledInvocation, CompiledInvocationBuilder, InvocationError};
use crate::invocations::misc::validate_under_limit;
use crate::invocations::{
    BuiltinInfo, CostValidationInfo, add_input_variables, get_non_fallthrough_statement_id,
};

/// Returns the Beta value of the Starkware elliptic curve.
fn get_beta() -> BigInt {
    BigInt::from_str("3141592653589793238462643383279502884197169399375105820974944592307816406665")
        .unwrap()
}

/// Builds instructions for Sierra EC operations.
pub fn build(
    libfunc: &EcConcreteLibfunc,
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    match libfunc {
        EcConcreteLibfunc::IsZero(_) => build_ec_point_is_zero(builder),
        EcConcreteLibfunc::Neg(_) => build_ec_neg(builder),
        EcConcreteLibfunc::StateAdd(_) => build_ec_state_add(builder),
        EcConcreteLibfunc::TryNew(_) => build_ec_point_try_new_nz(builder),
        EcConcreteLibfunc::StateFinalize(_) => build_ec_state_finalize(builder),
        EcConcreteLibfunc::StateInit(_) => build_ec_state_init(builder),
        EcConcreteLibfunc::StateAddMul(_) => build_ec_state_add_mul(builder),
        EcConcreteLibfunc::PointFromX(_) => build_ec_point_from_x_nz(builder),
        EcConcreteLibfunc::UnwrapPoint(_) => build_ec_point_unwrap(builder),
        EcConcreteLibfunc::Zero(_) => build_ec_zero(builder),
    }
}

/// Extends the CASM builder to include computation of `y^2` and `x^3 + x + BETA` for the given
/// pair (x, y). Populates the two "output vars" with the computed LHS and RHS of the EC equation.
fn compute_ec_equation(
    casm_builder: &mut CasmBuilder,
    x: Var,
    y: Var,
    auxiliary_vars: [Var; 3],
    computed_lhs: Var,
    computed_rhs: Var,
) {
    compute_lhs(casm_builder, y, computed_lhs);
    compute_rhs(casm_builder, x, auxiliary_vars, computed_rhs);
}

/// Computes the left-hand side of the EC equation, namely `y^2`.
fn compute_lhs(casm_builder: &mut CasmBuilder, y: Var, computed_lhs: Var) {
    casm_build_extend! {casm_builder,
        assert computed_lhs = y * y;
    };
}

/// Computes the right-hand side of the EC equation, namely `x^3 + x + BETA`.
fn compute_rhs(
    casm_builder: &mut CasmBuilder,
    x: Var,
    [x2, x3, alpha_x_plus_beta]: [Var; 3],
    computed_rhs: Var,
) {
    casm_build_extend! {casm_builder,
        const beta = (get_beta());
        assert x2 = x * x;
        assert x3 = x2 * x;
        assert alpha_x_plus_beta = x + beta; // Here we use the fact that Alpha is 1.
        assert computed_rhs = x3 + alpha_x_plus_beta;
    };
}

/// Extends the CASM builder to compute the sum, or difference, of two EC points, and store the
/// result in the given variables.
/// The inputs to the function are:
/// 1. The first point (`p0`).
/// 2. The X coordinate of the second point (`x1`).
/// 3. The "numerator", which is either `y0 - y1` (for point addition) or `y0 + y1` (for point
///    subtraction).
/// 4. The computation of `x0 - x1` (called "denominator"). Assumed to be non-zero.
fn add_ec_points_inner(
    casm_builder: &mut CasmBuilder,
    p0: (Var, Var),
    x1: Var,
    numerator: Var,
    denominator: Var,
) -> (Var, Var) {
    let (x0, y0) = p0;

    casm_build_extend! {casm_builder,
        tempvar slope = numerator / denominator;
        tempvar slope2 = slope * slope;
        tempvar sum_x = x0 + x1;

        tempvar x_diff;
        tempvar slope_times_x_change;
        // The result should be at the top of the stack.
        tempvar result_x;
        tempvar result_y;

        assert result_x = slope2 - sum_x;
        assert x_diff = x0 - result_x;
        assert slope_times_x_change = slope * x_diff;
        assert result_y = slope_times_x_change - y0;
    };

    (result_x, result_y)
}

/// Generates casm instructions for `ec_point_zero()`.
fn build_ec_zero(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let mut casm_builder = CasmBuilder::default();

    casm_build_extend!(casm_builder,
        const zero = 0;
    );

    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[zero, zero]], None)],
        Default::default(),
    ))
}

/// Handles instruction for creating an EC point.
fn build_ec_point_try_new_nz(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [x, y] = builder.try_get_single_cells()?;

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref x;
        deref y;
    };

    // Check if `(x, y)` is on the curve, by computing `y^2` and `x^3 + x + beta`.
    casm_build_extend! {casm_builder,
        tempvar y2;
        tempvar expected_y2;
        tempvar aux0;
        tempvar aux1;
        tempvar aux2;
    };
    compute_ec_equation(&mut casm_builder, x, y, [aux0, aux1, aux2], y2, expected_y2);
    casm_build_extend! {casm_builder,
        tempvar diff = y2 - expected_y2;
        jump NotOnCurve if diff != 0;
    };

    let failure_handle = get_non_fallthrough_statement_id(&builder);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[x, y]], None), ("NotOnCurve", &[], Some(failure_handle))],
        Default::default(),
    ))
}

/// Handles instruction for creating an EC point.
fn build_ec_point_from_x_nz(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [range_check, x] = builder.try_get_single_cells()?;

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        buffer(2) range_check;
        deref x;
    };

    casm_build_extend! {casm_builder,
        let orig_range_check = range_check;
        tempvar rhs;
        tempvar aux0;
        tempvar aux1;
        tempvar aux2;
    };
    compute_rhs(&mut casm_builder, x, [aux0, aux1, aux2], rhs);

    // Guess y, by either computing the square root of `rhs`, or of `3 * rhs`.
    casm_build_extend! {casm_builder,
        tempvar y;
        hint FieldSqrt {val: rhs} into {sqrt: y};
        tempvar lhs;
    };
    compute_lhs(&mut casm_builder, y, lhs);

    casm_build_extend! {casm_builder,
        tempvar diff = lhs - rhs;
        // If `(x, y)` is on the curve, return it.
        jump VerifyNotOnCurve if diff != 0;
        jump OnCurve;
        VerifyNotOnCurve:
        // Check that `y^2 = 3 * rhs`.
        const three = (3);
        assert lhs = rhs * three;
        // Note that `rhs != 0`: otherwise `y^2 = 3 * rhs = 0` implies `y = 0` and we would have
        // `y^2 = rhs` so this branch wouldn't have been chosen.
        // Alternatively, note that `rhs = 0` is not possible in curves of odd order (such as this
        // curve).
        // Since 3 is not a quadratic residue in the field, it will follow that `rhs` is not a
        // quadratic residue, which implies that there is no `y` such that `(x, y)` is on the curve.
        jump NotOnCurve;

        OnCurve:
    };

    // Check that y < PRIME / 2 to enforce a deterministic behavior (otherwise, the prover can
    // choose either y or -y).
    let auxiliary_vars: [_; 4] = std::array::from_fn(|_| casm_builder.alloc_var(false));
    validate_under_limit::<1>(
        &mut casm_builder,
        // Note that `1/2 (mod PRIME) = (PRIME + 1) / 2 = ceil(PRIME / 2)`.
        // Thus, `y < 1/2 (mod PRIME)` if and only if `y < PRIME / 2`.
        &(Felt252::ONE.field_div(&NonZeroFelt::TWO)).to_bigint(),
        y,
        range_check,
        &auxiliary_vars,
    );

    // Fallthrough - success.

    let not_on_curve = get_non_fallthrough_statement_id(&builder);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [
            ("Fallthrough", &[&[range_check], &[x, y]], None),
            ("NotOnCurve", &[&[range_check]], Some(not_on_curve)),
        ],
        CostValidationInfo {
            builtin_infos: vec![BuiltinInfo {
                cost_token_ty: CostTokenType::RangeCheck,
                start: orig_range_check,
                end: range_check,
            }],
            extra_costs: None,
        },
    ))
}

/// Handles instruction for unwrapping an EC point.
fn build_ec_point_unwrap(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [x, y] = builder.try_get_refs::<1>()?[0].try_unpack()?;

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref x;
        deref y;
    };

    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[x], &[y]], None)],
        Default::default(),
    ))
}

/// Generates casm instructions for `ec_point_is_zero()`.
fn build_ec_point_is_zero(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [x, y] = builder.try_get_refs::<1>()?[0].try_unpack()?;

    let mut casm_builder = CasmBuilder::default();
    add_input_variables!(casm_builder, deref x; deref y; );
    casm_build_extend! {casm_builder,
        // To check whether `(x, y) = (0, 0)` (the zero point), it is enough to check
        // whether `y = 0`, since there is no point on the curve with y = 0.
        jump Target if y != 0;
    };

    let target_statement_id = get_non_fallthrough_statement_id(&builder);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[], None), ("Target", &[&[x, y]], Some(target_statement_id))],
        Default::default(),
    ))
}

/// Generates casm instructions for `ec_neg()`.
fn build_ec_neg(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [x, y] = builder.try_get_refs::<1>()?[0].try_unpack()?;

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref x;
        deref y;
    };
    casm_build_extend!(casm_builder,
        const neg_one = -1;
        let neg_y = y * neg_one;
    );

    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[x, neg_y]], None)],
        Default::default(),
    ))
}

/// Handles instruction for initializing an EC state.
fn build_ec_state_init(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let mut casm_builder = CasmBuilder::default();

    // Sample a random point on the curve.
    casm_build_extend! {casm_builder,
        // Auxiliary variables.
        tempvar y2;
        tempvar aux0;
        tempvar aux1;
        tempvar aux2;

        // EC state variables (need to be at the top of the stack).
        tempvar random_x;
        tempvar random_y;
        tempvar random_ptr;
        hint RandomEcPoint into { x: random_x, y: random_y };
        // Initialize `random_ptr` and copy the random point into it.
        const ec_point_size = 2;
        hint AllocConstantSize { size: ec_point_size } into {dst: random_ptr};
        assert random_x = random_ptr[0];
        assert random_y = random_ptr[1];
    }

    // Assert that the random point is on the curve by passing `y2' as both
    // the left and right hand side of the curve equation.
    compute_ec_equation(&mut casm_builder, random_x, random_y, [aux0, aux1, aux2], y2, y2);

    // The third entry in the EC state is a pointer to the sampled random EC point.
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[random_x, random_y, random_ptr]], None)],
        Default::default(),
    ))
}

/// Handles instruction for adding a point to an EC state.
fn build_ec_state_add(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [expr_state, expr_point] = builder.try_get_refs()?;
    let [sx, sy, random_ptr] = expr_state.try_unpack()?;
    let [px, py] = expr_point.try_unpack()?;

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref px;
        deref py;
        deref sx;
        deref sy;
        deref random_ptr;
    };

    casm_build_extend! {casm_builder,
        // If the X coordinate is the same, either the points are equal or their sum is the point at
        // infinity. Either way, we can't compute the slope in this case.
        tempvar denominator = px - sx;
        jump NotSameX if denominator != 0;
        // X coordinate is identical; either the sum of the points is the point at infinity (not
        // allowed), or the points are equal, which is also not allowed (doubling).
        // Since the base state should be random - the only way to get the same point twice is if
        // the prover chose a non-random state, so we explicitly fail.
        fail;
        NotSameX:
        tempvar numerator = py - sy;
    };

    let (result_x, result_y) =
        add_ec_points_inner(&mut casm_builder, (px, py), sx, numerator, denominator);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[result_x, result_y, random_ptr]], None)],
        Default::default(),
    ))
}

/// Handles instruction for finalizing an EC state.
fn build_ec_state_finalize(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [x, y, random_ptr] = builder.try_get_refs::<1>()?[0].try_unpack()?;

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref x;
        deref y;
        deref random_ptr;
    };

    // We want to return the point `(x, y) - (random_x, random_y)`, or in other words,
    // `(x, y) + (random_x, -random_y)`.
    casm_build_extend! {casm_builder,
        tempvar random_x = random_ptr[0];
        tempvar random_y = random_ptr[1];
        // If the X coordinate is the same, either the points are equal or their sum is the point at
        // infinity. Either way, we can't compute the slope in this case.
        // The result may be the point at infinity if the user called `ec_state_try_finalize_nz`
        // immediately after ec_state_init.
        tempvar denominator = x - random_x;
        jump NotSameX if denominator != 0;
        // Assert the result is the point at infinity (the other option is the points are the same,
        // and doubling is not allowed).
        // Since the base state should be random - the only way to get the same point twice is if
        // the prover chose a non-random state.
        assert y = random_y;
        jump SumIsInfinity;
        NotSameX:
        // The numerator is the difference in Y coordinate values of the summed points, and the Y
        // coordinate of the negated random point is `-random_y`.
        tempvar numerator = y + random_y;
    }

    let result_x_y: [Var; 2] =
        add_ec_points_inner(&mut casm_builder, (x, y), random_x, numerator, denominator).into();

    let failure_handle = get_non_fallthrough_statement_id(&builder);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&result_x_y], None), ("SumIsInfinity", &[], Some(failure_handle))],
        Default::default(),
    ))
}

/// Handles instruction for computing `S + M * Q` where `S` is an EC state, `M` is a scalar
/// (felt252) and `Q` is an EC point.
fn build_ec_state_add_mul(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [ec_builtin_expr, expr_state, expr_m, expr_point] = builder.try_get_refs()?;
    let ec_builtin = ec_builtin_expr.try_unpack_single()?;
    let [sx, sy, random_ptr] = expr_state.try_unpack()?;
    let [m] = expr_m.try_unpack()?;
    let [px, py] = expr_point.try_unpack()?;

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        buffer(6) ec_builtin;
        deref sx;
        deref sy;
        deref random_ptr;
        deref px;
        deref py;
        deref m;
    };
    casm_build_extend! {casm_builder,
        assert sx = *(ec_builtin++);
        assert sy = *(ec_builtin++);
        assert px = *(ec_builtin++);
        assert py = *(ec_builtin++);
        assert m = *(ec_builtin++);
        let result_x = *(ec_builtin++);
        let result_y = *(ec_builtin++);
    };
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[ec_builtin], &[result_x, result_y, random_ptr]], None)],
        Default::default(),
    ))
}
