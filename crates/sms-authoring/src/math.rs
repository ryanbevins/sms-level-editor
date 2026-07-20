use crate::{AuthoringError, AuthoringResult, CoordinateConversion};

pub(crate) type Matrix4 = [[f32; 4]; 4];

pub(crate) fn validate_conversion(conversion: &CoordinateConversion) -> AuthoringResult<()> {
    if !conversion.units_per_meter.is_finite() || conversion.units_per_meter <= 0.0 {
        return Err(AuthoringError::invalid(
            "units_per_meter must be finite and greater than zero",
        ));
    }
    if conversion
        .basis
        .iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(AuthoringError::invalid(
            "coordinate basis contains a non-finite value",
        ));
    }
    inverse3(conversion.basis)
        .ok_or_else(|| AuthoringError::invalid("coordinate basis is singular"))?;
    Ok(())
}

pub(crate) fn conversion_matrix(conversion: &CoordinateConversion) -> Matrix4 {
    let mut matrix = identity();
    for (column, values) in matrix.iter_mut().take(3).enumerate() {
        for (row, value) in values.iter_mut().take(3).enumerate() {
            *value = conversion.basis[row][column] * conversion.units_per_meter;
        }
    }
    matrix
}

pub(crate) fn inverse_conversion_matrix(
    conversion: &CoordinateConversion,
) -> AuthoringResult<Matrix4> {
    let inverse = inverse3(conversion.basis)
        .ok_or_else(|| AuthoringError::invalid("coordinate basis is singular"))?;
    let mut matrix = identity();
    for (column, values) in matrix.iter_mut().take(3).enumerate() {
        for (row, value) in values.iter_mut().take(3).enumerate() {
            *value = inverse[row][column] / conversion.units_per_meter;
        }
    }
    Ok(matrix)
}

pub(crate) fn convert_local_transform(
    local: Matrix4,
    conversion: &CoordinateConversion,
) -> AuthoringResult<Matrix4> {
    Ok(mul(
        mul(conversion_matrix(conversion), local),
        inverse_conversion_matrix(conversion)?,
    ))
}

pub(crate) fn convert_position(position: [f32; 3], conversion: &CoordinateConversion) -> [f32; 3] {
    let transformed = mul3_vec(conversion.basis, position);
    [
        transformed[0] * conversion.units_per_meter,
        transformed[1] * conversion.units_per_meter,
        transformed[2] * conversion.units_per_meter,
    ]
}

pub(crate) fn convert_normal(
    normal: [f32; 3],
    conversion: &CoordinateConversion,
) -> AuthoringResult<[f32; 3]> {
    let inverse = inverse3(conversion.basis)
        .ok_or_else(|| AuthoringError::invalid("coordinate basis is singular"))?;
    Ok(normalize(mul3_vec(transpose3(inverse), normal)))
}

pub(crate) fn convert_tangent(tangent: [f32; 4], conversion: &CoordinateConversion) -> [f32; 4] {
    let direction = normalize(mul3_vec(
        conversion.basis,
        [tangent[0], tangent[1], tangent[2]],
    ));
    let handedness = if conversion.reverse_winding {
        -tangent[3]
    } else {
        tangent[3]
    };
    [direction[0], direction[1], direction[2], handedness]
}

pub(crate) fn identity() -> Matrix4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

pub(crate) fn mul(left: Matrix4, right: Matrix4) -> Matrix4 {
    let mut result = [[0.0; 4]; 4];
    for column in 0..4 {
        for row in 0..4 {
            result[column][row] = (0..4)
                .map(|component| left[component][row] * right[column][component])
                .sum();
        }
    }
    result
}

pub(crate) fn transform_point(matrix: Matrix4, point: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * point[0] + matrix[1][0] * point[1] + matrix[2][0] * point[2] + matrix[3][0],
        matrix[0][1] * point[0] + matrix[1][1] * point[1] + matrix[2][1] * point[2] + matrix[3][1],
        matrix[0][2] * point[0] + matrix[1][2] * point[1] + matrix[2][2] * point[2] + matrix[3][2],
    ]
}

pub(crate) fn transform_normal(matrix: Matrix4, normal: [f32; 3]) -> AuthoringResult<[f32; 3]> {
    let linear = matrix_linear(matrix);
    let inverse = inverse3(linear)
        .ok_or_else(|| AuthoringError::invalid("node transform has a singular linear basis"))?;
    Ok(normalize(mul3_vec(transpose3(inverse), normal)))
}

pub(crate) fn transform_tangent_frame(
    matrix: Matrix4,
    normal: [f32; 3],
    tangent: [f32; 4],
) -> AuthoringResult<[[f32; 3]; 3]> {
    let linear = matrix_linear(matrix);
    let transformed_normal = transform_normal(matrix, normal)?;
    let raw_tangent = mul3_vec(linear, [tangent[0], tangent[1], tangent[2]]);
    let projection = dot(raw_tangent, transformed_normal);
    let orthogonal = sub(
        raw_tangent,
        transformed_normal.map(|component| component * projection),
    );
    if dot(orthogonal, orthogonal) == 0.0 {
        return Err(AuthoringError::invalid(
            "node transform collapses a tangent direction",
        ));
    }
    let transformed_tangent = normalize(orthogonal);
    let handedness = tangent[3].signum() * determinant3(linear).signum();
    let binormal = normalize(
        cross(transformed_normal, transformed_tangent).map(|component| component * handedness),
    );
    Ok([transformed_normal, binormal, transformed_tangent])
}

pub(crate) fn transform_reverses_winding(matrix: Matrix4) -> bool {
    determinant3(matrix_linear(matrix)).is_sign_negative()
}

pub(crate) fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

pub(crate) fn sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

pub(crate) fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

pub(crate) fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let length_squared = vector
        .iter()
        .map(|component| component * component)
        .sum::<f32>();
    if length_squared == 0.0 || !length_squared.is_finite() {
        [0.0, 1.0, 0.0]
    } else {
        let inverse_length = length_squared.sqrt().recip();
        [
            vector[0] * inverse_length,
            vector[1] * inverse_length,
            vector[2] * inverse_length,
        ]
    }
}

fn mul3_vec(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

fn transpose3(matrix: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    [
        [matrix[0][0], matrix[1][0], matrix[2][0]],
        [matrix[0][1], matrix[1][1], matrix[2][1]],
        [matrix[0][2], matrix[1][2], matrix[2][2]],
    ]
}

fn inverse3(matrix: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let determinant = determinant3(matrix);
    if determinant == 0.0 || !determinant.is_finite() {
        return None;
    }
    let inverse_determinant = determinant.recip();
    Some([
        [
            (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1]) * inverse_determinant,
            (matrix[0][2] * matrix[2][1] - matrix[0][1] * matrix[2][2]) * inverse_determinant,
            (matrix[0][1] * matrix[1][2] - matrix[0][2] * matrix[1][1]) * inverse_determinant,
        ],
        [
            (matrix[1][2] * matrix[2][0] - matrix[1][0] * matrix[2][2]) * inverse_determinant,
            (matrix[0][0] * matrix[2][2] - matrix[0][2] * matrix[2][0]) * inverse_determinant,
            (matrix[0][2] * matrix[1][0] - matrix[0][0] * matrix[1][2]) * inverse_determinant,
        ],
        [
            (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]) * inverse_determinant,
            (matrix[0][1] * matrix[2][0] - matrix[0][0] * matrix[2][1]) * inverse_determinant,
            (matrix[0][0] * matrix[1][1] - matrix[0][1] * matrix[1][0]) * inverse_determinant,
        ],
    ])
}

fn determinant3(matrix: [[f32; 3]; 3]) -> f32 {
    matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0])
}

fn matrix_linear(matrix: Matrix4) -> [[f32; 3]; 3] {
    [
        [matrix[0][0], matrix[1][0], matrix[2][0]],
        [matrix[0][1], matrix[1][1], matrix[2][1]],
        [matrix[0][2], matrix[1][2], matrix[2][2]],
    ]
}
