import pathlib
import tempfile
from logging import getLogger

import grepify

_logger = getLogger(__name__)

_tmp_db_path_base = pathlib.Path(tempfile.mkdtemp()) / "grepify_test"
_logger.info("Temporary database path base: %s", _tmp_db_path_base)


def get_env_db_path(name: str) -> pathlib.Path:
    return _tmp_db_path_base / name


_PATH_PREFIX = str(pathlib.Path(__file__).parent.parent) + "/"


def create_test_env(
    test_file_path: str,
    suffix: str | None = None,
    *,
    exception_handler: grepify.ExceptionHandler | None = None,
) -> grepify.Environment:
    base_name = (
        test_file_path.removeprefix(_PATH_PREFIX).removesuffix(".py").replace("/", "__")
    )
    if suffix is not None:
        base_name = f"{base_name}__{suffix}"
    settings = grepify.Settings.from_env(db_path=get_env_db_path(base_name))
    return grepify.Environment(settings, exception_handler=exception_handler)
