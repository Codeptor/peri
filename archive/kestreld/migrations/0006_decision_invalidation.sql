-- falsifiable decision plans (2026-08-24): the analyst's stated invalidation condition,
-- persisted so review/forensics can see the falsifier the model committed to. NULL for
-- pre-migration rows and for decisions the model left unstated.
ALTER TABLE decisions ADD COLUMN invalidation_condition TEXT;
