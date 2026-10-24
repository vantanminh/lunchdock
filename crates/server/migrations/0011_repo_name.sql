-- The source repository is named rudder. Panels that still have the previous
-- default follow it. A repository someone set by hand is left alone.
-- GitHub still has to serve that name; until the project is renamed there,
-- set the repository back under System → Updates or RUDDER_GITHUB_REPO.

UPDATE panel_settings
SET github_repo = 'vantanminh/rudder'
WHERE github_repo = 'vantanminh/lunchdock';

ALTER TABLE panel_settings
    ALTER COLUMN github_repo SET DEFAULT 'vantanminh/rudder';
