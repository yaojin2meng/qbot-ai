-- 清理未使用的表：api_configs 和 model_tests 在代码中完全没有用到
DROP TABLE IF EXISTS model_tests;
DROP TABLE IF EXISTS api_configs;
