use bikesnest_test_support::run_isolated_unmigrated_database_test;
use sqlx::migrate::Migrator;
use std::borrow::Cow;

static CURRENT: Migrator = sqlx::migrate!("../../migrations");

#[test]
fn committed_0027_schema_upgrades_to_current_without_losing_policy_data() {
    run_isolated_unmigrated_database_test(|pool: sqlx::PgPool| async move {
        let pre_terms = Migrator {
            migrations: Cow::Owned(
                CURRENT
                    .iter()
                    .filter(|migration| migration.version <= 27)
                    .cloned()
                    .collect(),
            ),
            ..Migrator::DEFAULT
        };
        pre_terms.run(&pool).await.unwrap();

        let before: (i64, i64, bool) = sqlx::query_as(
            "SELECT count(*), max(version), bool_and(success) FROM _sqlx_migrations",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, (27, 27, true));

        let legacy_id: i64 = sqlx::query_scalar(
            "INSERT INTO policy_version(kind,locale,version,effective_at,content) \
             VALUES('terms','en','pre-terms','2026-01-01','preserve me') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        CURRENT.run(&pool).await.unwrap();

        let after: (i64, i64, bool) = sqlx::query_as(
            "SELECT count(*), max(version), bool_and(success) FROM _sqlx_migrations",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let latest = CURRENT.iter().map(|m| m.version).max().unwrap();
        assert_eq!(after, (CURRENT.iter().count() as i64, latest, true));
        let preserved: (i64, String, bool) = sqlx::query_as(
            "SELECT id,content,requires_acknowledgement FROM policy_version WHERE id=$1",
        )
        .bind(legacy_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(preserved, (legacy_id, "preserve me".into(), false));

        let proof_tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' \
             AND table_name IN ('terms_acknowledgement','terms_notice_presentation')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(proof_tables, 2);
    });
}
