-- The demo video compares public with this: the same forum a release behind.
CREATE SCHEMA staging;
CREATE TABLE staging.categories (LIKE public.categories INCLUDING ALL);
CREATE TABLE staging.users (LIKE public.users INCLUDING ALL);
ALTER TABLE staging.users DROP COLUMN preferences, DROP COLUMN tags;
CREATE TABLE staging.posts (LIKE public.posts INCLUDING ALL);
ALTER TABLE staging.posts DROP COLUMN metadata, ALTER COLUMN title TYPE character varying(120);

-- A trigger for the tour to open in the schema tree.
CREATE FUNCTION public.normalize_email() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.email := lower(NEW.email);
    RETURN NEW;
END;
$$;
CREATE TRIGGER users_normalize_email BEFORE INSERT OR UPDATE OF email ON public.users
    FOR EACH ROW EXECUTE FUNCTION public.normalize_email();
