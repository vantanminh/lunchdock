-- Load guard: keep the panel host answering while builds and app traffic load it.
-- New builds wait while free memory is below the reserve or CPU stays above the
-- limit; running builds stop when free memory drops below half the reserve.
-- `builds_paused` holds every new build until an admin resumes them.

ALTER TABLE panel_settings
    ADD COLUMN guard_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN guard_memory_reserve_percent SMALLINT NOT NULL DEFAULT 15
        CONSTRAINT panel_settings_guard_memory CHECK (guard_memory_reserve_percent BETWEEN 5 AND 50),
    ADD COLUMN guard_cpu_percent SMALLINT NOT NULL DEFAULT 90
        CONSTRAINT panel_settings_guard_cpu CHECK (guard_cpu_percent BETWEEN 50 AND 100),
    ADD COLUMN builds_paused BOOLEAN NOT NULL DEFAULT FALSE;
