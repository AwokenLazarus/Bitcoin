def pytest_configure(config):
    config.addinivalue_line("markers", "live: reads live services or data")
