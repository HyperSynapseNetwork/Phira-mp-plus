-- Clean up orphaned tables from removed Simulation feature
DROP TABLE IF EXISTS mp_sim_events;

-- Drop simulation-related columns from benchmark tables
ALTER TABLE mp_runtime_benchmark_reports DROP COLUMN IF EXISTS mode;
ALTER TABLE mp_runtime_benchmark_reports DROP COLUMN IF EXISTS is_simulation;
