import io
import logging
import os
import time
from typing import Literal

import ts_async_api
from openai import AsyncOpenAI
from pydantic import Field
from pydantic_settings import BaseSettings, CliApp

client = AsyncOpenAI()

LOGGER = logging.getLogger(__name__)

async def parse(audio: bytes) -> None:
    start = time.perf_counter()  # 高精度计时
    res = await client.audio.transcriptions.create(
        model="local/whisper-large-v3-turbo-zh-ct2-int8-float16",
        file=io.BytesIO(audio),
        language="zh",
        response_format="verbose_json",
    )
    LOGGER.info(f"Detected language {res.language}")

    for segment in res.segments:
        LOGGER.info("[%.2fs -> %.2fs] %s" % (segment.start, segment.end, segment.text))
    mid = time.perf_counter()
    LOGGER.info(f"耗时: {mid - start:.6f} 秒")

def init_logger(
    log_level: Literal["CRITICAL", "FATAL", "ERROR", "WARN", "INFO", "DEBUG"] = "DEBUG",
) -> None:
    """初始化日志"""
    logging.root.setLevel(level=log_level)
    if "PYTEST_VERSION" not in os.environ:
        log_format = "%(asctime)s - %(name)s - %(levelname)s - %(message)s (%(filename)s:%(lineno)d)"
        formatter = logging.Formatter(log_format)
        stream_handler = logging.StreamHandler()
        stream_handler.setFormatter(formatter)
        logging.root.addHandler(stream_handler)

class AsyncSettings(BaseSettings, cli_enforce_required=True):
    """args"""

    address: str = Field(description="服务端域名/IP:服务端端口")
    server_password: str = Field(description="服务器密码")
    identity: str = Field(description="identity")
    client_nickname: str = Field(description="bot 在 ts 中显示的名称")
    log_level: Literal["CRITICAL", "FATAL", "ERROR", "WARN", "INFO", "DEBUG"] = Field(
        default="INFO", description="日志等级"
    )

    async def cli_cmd(self) -> None:
        """Ts server query client main"""
        init_logger(log_level=self.log_level)

        bot = await ts_async_api.TsBot.connect(
            address=self.address,
            identity=self.identity,
            name=self.client_nickname,
            password=self.server_password,
        )
        async with bot:
            while True:
                client_id, audio = await bot.wait_audio()
                LOGGER("client id %d", client_id)
                await parse(audio)
        LOGGER.info("end")




if __name__ == "__main__":
    CliApp.run(AsyncSettings)
