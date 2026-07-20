param(
    [Parameter(Mandatory = $true)]
    [string]$BaseRoot
)

$ErrorActionPreference = "Stop"

$resolvedBaseRoot = (Resolve-Path -LiteralPath $BaseRoot).Path
if (-not (Test-Path -LiteralPath $resolvedBaseRoot -PathType Container)) {
    throw "Retail base root is not a directory: $resolvedBaseRoot"
}

$environmentNames = @(
    "SMS_BASE_ROOT",
    "SMS_NOKI_TEST_BASE_ROOT",
    "SMS_WIRE_TEST_BASE_ROOT",
    "SMS_GRASS_TEST_BASE_ROOT",
    "SMS_FLAG_TEST_BASE_ROOT"
)
$previousEnvironment = @{}
foreach ($name in $environmentNames) {
    $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
    [Environment]::SetEnvironmentVariable($name, $resolvedBaseRoot, "Process")
}

$repoRoot = Split-Path -Parent $PSScriptRoot
Write-Host "SMS retail preview regression suite: Japanese revision (GMSJ01)"
Push-Location $repoRoot
try {
    $tests = @(
        @("test", "-p", "sms-schema", "tests::generated_neighboring_decomp_schema_satisfies_registry_invariants", "--", "--ignored", "--exact"),
        @("test", "-p", "sms-scene", "tests::audits_all_retail_map_obj_resource_previews", "--", "--ignored", "--exact"),
        @("test", "-p", "sms-scene", "tests::retail_map_obj_resource_selectors_keep_instance_models", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "tests::retail_nozzle_boxes_keep_typed_items_and_tev_colors", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "noki_render_test::retail_nozzle_colors_and_red_pepper_offsets_reach_rendered_materials_and_geometry", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "noki_render_test::retail_map_obj_indirect_flags_render_all_dokan_gate_and_ice_block_placements", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "noki_render_test::retail_mare_pollution_uses_map_static_flags_and_keeps_collision_only_rows_model_less", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "noki_render_test::retail_surf_geso_overrides_render_all_variants_with_runtime_colors", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "noki_render_test::retail_stage_preview_matrix_keeps_environment_and_instance_models", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "preview_wires::tests::representative_stages_load_their_wire_tables_when_assets_are_available", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "preview_flags::tests::dolpic_flags_use_retail_bti_assets_when_available", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "preview_grass::tests::dolpic_ex3_builds_all_retail_grass_when_assets_are_available", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "noki_render_test::bianco_water_pollution_model_follows_map_static_placement", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "noki_render_test::bianco_mirror_surface_follows_runtime_cube_volume", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "noki_render_test::retail_reflect_sky_helpers_never_enter_the_main_viewport", "--", "--ignored", "--exact"),
        @("test", "-p", "graffito-editor", "noki_render_test::mamma0_ocean_water_survives_enemy_preview_catalog", "--", "--ignored", "--exact")
    )

    foreach ($arguments in $tests) {
        $lockedArguments = @($arguments[0], "--locked") + $arguments[1..($arguments.Count - 1)]
        & cargo @lockedArguments
        if ($LASTEXITCODE -ne 0) {
            throw "cargo $($lockedArguments -join ' ') failed with exit code $LASTEXITCODE"
        }
    }
}
finally {
    Pop-Location
    foreach ($name in $environmentNames) {
        [Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name], "Process")
    }
}
