-- The role and database `catalog-engine` provisions itself into, created as superuser.

do $$
begin
    if not exists (
        select from pg_catalog.pg_roles where rolname = 'catalog_engine_user'
    ) then
        create user catalog_engine_user createdb;
    end if;
end
$$;

select 'CREATE DATABASE catalog_engine OWNER catalog_engine_user'
where not exists (
    select from pg_catalog.pg_database where datname = 'catalog_engine'
)\gexec
