# Convenience targets. CI uses the same scripts (.github/workflows/duckdb*.yml).
.PHONY: release wasm wasm_eh test test_versions test_wasm repository

release:            ## native extension for this machine -> build/<platform>/
	./build.sh

wasm:               ## DuckDB-Wasm extensions wasm_eh, wasm_mvp, wasm_threads (emcc, Rust nightly + rust-src)
	PLATFORM=all ./build_wasm.sh

wasm_eh:            ## DuckDB-Wasm extension for the eh/base bundle only -> build/wasm_eh/
	./build_wasm.sh

test:               ## Rust unit tests + spec vectors on the installed duckdb package
	cargo test --release
	python3 tests/test_vectors.py build/$${DUCKDB_PLATFORM:-linux_amd64}/geosquare_v2.duckdb_extension

test_versions:      ## vectors + datasets on DuckDB 1.4.0 ... 2.0.0 (venvs cached in .venvs)
	TESTS="tests/test_vectors.py tests/test_datasets.py tests/test_iceberg_planned.py tests/test_fill_nulls.py tests/test_docs_examples.py tests/test_multi_db.py tests/test_detect.py tests/test_legacy_profile.py tests/test_rate_rollup.py tests/test_rollup.py tests/test_v1.py tests/test_multi_query.py tests/test_write_rollup.py tests/test_expire.py tests/test_compact.py tests/test_keep_metadata.py tests/test_time_travel.py tests/test_radius_wkb.py tests/test_inspect.py tests/test_upsert.py tests/test_alter.py tests/test_raster.py" tests/run_versions.sh

test_wasm:          ## DuckDB-Wasm in headless Chromium vs native
	tests/wasm/run.sh

repository:         ## extension repository from dist/<platform>/ for all DuckDB releases
	python3 scripts/make_repository.py --artifacts dist --out repo --versions auto
