-- OpenSearch 搜索投影增量事件。
-- PostgreSQL 是事实源；触发器只写事务内 Outbox，不执行任何网络调用。

CREATE OR REPLACE FUNCTION enqueue_catalog_edition(p_edition_id UUID, p_event_type TEXT)
RETURNS VOID AS $$
BEGIN
    IF p_edition_id IS NOT NULL THEN
        INSERT INTO catalog_outbox (event_type, aggregate_type, aggregate_id, payload, status)
        VALUES (p_event_type, 'edition', p_edition_id, '{}'::jsonb, '待同步');
    END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION trg_catalog_edition_changed()
RETURNS TRIGGER AS $$
BEGIN
    PERFORM enqueue_catalog_edition(CASE WHEN TG_OP = 'DELETE' THEN OLD.id ELSE NEW.id END,
                                    'catalog.edition_changed');
    IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION trg_catalog_edition_ref_changed()
RETURNS TRIGGER AS $$
BEGIN
    PERFORM enqueue_catalog_edition(CASE WHEN TG_OP = 'DELETE' THEN OLD.edition_id ELSE NEW.edition_id END,
                                    'catalog.edition_relation_changed');
    IF TG_OP = 'UPDATE' AND OLD.edition_id IS DISTINCT FROM NEW.edition_id THEN
        PERFORM enqueue_catalog_edition(OLD.edition_id, 'catalog.edition_relation_changed');
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION trg_catalog_identifier_changed()
RETURNS TRIGGER AS $$
DECLARE
    target_type TEXT;
    target_id UUID;
BEGIN
    target_type := CASE WHEN TG_OP = 'DELETE' THEN OLD.object_type ELSE NEW.object_type END;
    target_id := CASE WHEN TG_OP = 'DELETE' THEN OLD.object_id ELSE NEW.object_id END;
    IF target_type = 'edition' THEN
        PERFORM enqueue_catalog_edition(target_id, 'catalog.identifier_changed');
    END IF;
    IF TG_OP = 'UPDATE' AND OLD.object_type = 'edition'
       AND (OLD.object_type, OLD.object_id) IS DISTINCT FROM (NEW.object_type, NEW.object_id) THEN
        PERFORM enqueue_catalog_edition(OLD.object_id, 'catalog.identifier_changed');
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION trg_catalog_contributor_changed()
RETURNS TRIGGER AS $$
DECLARE
    edition UUID;
BEGIN
    FOR edition IN SELECT ec.edition_id FROM edition_contributors ec
                   WHERE ec.contributor_id = CASE WHEN TG_OP = 'DELETE' THEN OLD.id ELSE NEW.id END
    LOOP
        PERFORM enqueue_catalog_edition(edition, 'catalog.contributor_changed');
    END LOOP;
    IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION trg_catalog_source_asset_changed()
RETURNS TRIGGER AS $$
DECLARE
    source_record UUID;
    edition UUID;
BEGIN
    source_record := CASE WHEN TG_OP = 'DELETE' THEN OLD.source_record_id ELSE NEW.source_record_id END;
    FOR edition IN SELECT rr.edition_id FROM record_resolutions rr
                   WHERE rr.source_record_id = source_record AND rr.edition_id IS NOT NULL
    LOOP
        PERFORM enqueue_catalog_edition(edition, 'catalog.source_asset_changed');
    END LOOP;
    IF TG_OP = 'UPDATE' AND OLD.source_record_id IS DISTINCT FROM NEW.source_record_id THEN
        FOR edition IN SELECT rr.edition_id FROM record_resolutions rr
                       WHERE rr.source_record_id = OLD.source_record_id AND rr.edition_id IS NOT NULL
        LOOP
            PERFORM enqueue_catalog_edition(edition, 'catalog.source_asset_changed');
        END LOOP;
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION trg_catalog_library_file_changed()
RETURNS TRIGGER AS $$
DECLARE
    edition UUID;
BEGIN
    FOR edition IN SELECT h.edition_id FROM holdings h
                   WHERE h.library_file_id = CASE WHEN TG_OP = 'DELETE' THEN OLD.id ELSE NEW.id END
    LOOP
        PERFORM enqueue_catalog_edition(edition, 'catalog.library_file_changed');
    END LOOP;
    IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION trg_catalog_execution_changed()
RETURNS TRIGGER AS $$
DECLARE
    target UUID;
    edition UUID;
BEGIN
    target := CASE WHEN TG_OP = 'DELETE' THEN OLD.target_id ELSE NEW.target_id END;
    SELECT at.edition_id INTO edition FROM acquisition_targets at WHERE at.id = target;
    PERFORM enqueue_catalog_edition(edition, 'catalog.acquisition_execution_changed');
    IF TG_OP = 'UPDATE' AND OLD.target_id IS DISTINCT FROM NEW.target_id THEN
        SELECT at.edition_id INTO edition FROM acquisition_targets at WHERE at.id = OLD.target_id;
        PERFORM enqueue_catalog_edition(edition, 'catalog.acquisition_execution_changed');
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS editions_catalog_outbox ON editions;
CREATE TRIGGER editions_catalog_outbox
AFTER INSERT OR UPDATE OR DELETE ON editions
FOR EACH ROW EXECUTE FUNCTION trg_catalog_edition_changed();

DROP TRIGGER IF EXISTS identifiers_catalog_outbox ON identifiers;
CREATE TRIGGER identifiers_catalog_outbox
AFTER INSERT OR UPDATE OR DELETE ON identifiers
FOR EACH ROW EXECUTE FUNCTION trg_catalog_identifier_changed();

DROP TRIGGER IF EXISTS edition_contributors_catalog_outbox ON edition_contributors;
CREATE TRIGGER edition_contributors_catalog_outbox
AFTER INSERT OR UPDATE OR DELETE ON edition_contributors
FOR EACH ROW EXECUTE FUNCTION trg_catalog_edition_ref_changed();

DROP TRIGGER IF EXISTS contributors_catalog_outbox ON contributors;
CREATE TRIGGER contributors_catalog_outbox
AFTER UPDATE OR DELETE ON contributors
FOR EACH ROW EXECUTE FUNCTION trg_catalog_contributor_changed();

DROP TRIGGER IF EXISTS record_resolutions_catalog_outbox ON record_resolutions;
CREATE TRIGGER record_resolutions_catalog_outbox
AFTER INSERT OR UPDATE OR DELETE ON record_resolutions
FOR EACH ROW EXECUTE FUNCTION trg_catalog_edition_ref_changed();

DROP TRIGGER IF EXISTS source_assets_catalog_outbox ON source_assets;
CREATE TRIGGER source_assets_catalog_outbox
AFTER INSERT OR UPDATE OR DELETE ON source_assets
FOR EACH ROW EXECUTE FUNCTION trg_catalog_source_asset_changed();

DROP TRIGGER IF EXISTS holdings_catalog_outbox ON holdings;
CREATE TRIGGER holdings_catalog_outbox
AFTER INSERT OR UPDATE OR DELETE ON holdings
FOR EACH ROW EXECUTE FUNCTION trg_catalog_edition_ref_changed();

DROP TRIGGER IF EXISTS library_files_catalog_outbox ON library_files;
CREATE TRIGGER library_files_catalog_outbox
AFTER UPDATE OR DELETE ON library_files
FOR EACH ROW EXECUTE FUNCTION trg_catalog_library_file_changed();

DROP TRIGGER IF EXISTS acquisition_targets_catalog_outbox ON acquisition_targets;
CREATE TRIGGER acquisition_targets_catalog_outbox
AFTER INSERT OR UPDATE OR DELETE ON acquisition_targets
FOR EACH ROW EXECUTE FUNCTION trg_catalog_edition_ref_changed();

DROP TRIGGER IF EXISTS acquisition_executions_catalog_outbox ON acquisition_executions;
CREATE TRIGGER acquisition_executions_catalog_outbox
AFTER INSERT OR UPDATE OR DELETE ON acquisition_executions
FOR EACH ROW EXECUTE FUNCTION trg_catalog_execution_changed();
