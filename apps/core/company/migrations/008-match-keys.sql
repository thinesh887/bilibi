-- Duplicate candidates (issue 139): the normalised facts a company is matched
-- on, stored as generated columns so they change in the same transaction as
-- the row. The functions are the one definition: the duplicate query applies
-- them to its own criteria, so a criterion and a stored key always normalise
-- alike. No extension is needed.

-- Lower-case words of letters and digits, Latin accents folded as the code
-- backfill of `004-organisation` folds them.
CREATE FUNCTION company_match_tokens(value text) RETURNS text[]
 LANGUAGE sql IMMUTABLE PARALLEL SAFE
 RETURN array_remove(regexp_split_to_array(lower(translate(coalesce(value, ''),'ÀÁÂÃÄÅàáâãäåÇçÈÉÊËèéêëÌÍÎÏìíîïÑñÒÓÔÕÖØòóôõöøÙÚÛÜùúûüÝýÿŠšŽžĆćČčĐđŁłŚśŹźŻżŃńĘęĄąŐőŰű','AAAAAAaaaaaaCcEEEEeeeeIIIIiiiiNnOOOOOOooooooUUUUuuuuYyySsZzCcCcDdLlSsZzZzNnEeAaOoUu')), '[^[:alnum:]]+'), '');

-- A website's host, without scheme, `www.`, port, path or trailing dot.
CREATE FUNCTION company_match_host(value text) RETURNS text
 LANGUAGE sql IMMUTABLE PARALLEL SAFE
 RETURN NULLIF(rtrim(regexp_replace(regexp_replace(lower(btrim(coalesce(value, ''))), '^[a-z][a-z0-9+.-]*://', ''), '^www\.|[/?#:].*$', '', 'g'), '.'), '');

-- An email address's domain.
CREATE FUNCTION company_match_domain(value text) RETURNS text
 LANGUAGE sql IMMUTABLE PARALLEL SAFE
 RETURN NULLIF(rtrim(lower(btrim(split_part(coalesce(value, ''), '@', 2))), '.'), '');

-- A phone number's digits, when there are at least seven.
CREATE FUNCTION company_match_digits(value text) RETURNS text
 LANGUAGE sql IMMUTABLE PARALLEL SAFE
 RETURN CASE WHEN length(regexp_replace(coalesce(value, ''), '[^0-9]', '', 'g')) >= 7 THEN regexp_replace(value, '[^0-9]', '', 'g') END;

-- A registration number's letters and digits, upper case.
CREATE FUNCTION company_match_registration(value text) RETURNS text
 LANGUAGE sql IMMUTABLE PARALLEL SAFE
 RETURN NULLIF(upper(regexp_replace(coalesce(value, ''), '[^[:alnum:]]', '', 'g')), '');

ALTER TABLE companies
 ADD COLUMN name_tokens text[] GENERATED ALWAYS AS (company_match_tokens(name || ' ' || coalesce(legal_name, ''))) STORED,
 ADD COLUMN website_host text GENERATED ALWAYS AS (company_match_host(website)) STORED,
 ADD COLUMN email_domain text GENERATED ALWAYS AS (company_match_domain(email)) STORED,
 ADD COLUMN phone_digits text GENERATED ALWAYS AS (company_match_digits(phone)) STORED,
 ADD COLUMN registration_key text GENERATED ALWAYS AS (company_match_registration(registration_number)) STORED;

-- The name words preselect candidates; each exact key has its own partial
-- index within the tenant.
CREATE INDEX companies_name_tokens ON companies USING gin (name_tokens);
CREATE INDEX companies_website_host ON companies(tenant_id, website_host) WHERE website_host IS NOT NULL;
CREATE INDEX companies_email_domain ON companies(tenant_id, email_domain) WHERE email_domain IS NOT NULL;
CREATE INDEX companies_phone_digits ON companies(tenant_id, phone_digits) WHERE phone_digits IS NOT NULL;
CREATE INDEX companies_registration_key ON companies(tenant_id, registration_key) WHERE registration_key IS NOT NULL;
