-- Start of the window the stored pull requests cover; NULL when unknown.
ALTER TABLE sync_status ADD COLUMN pr_window_start TEXT;
