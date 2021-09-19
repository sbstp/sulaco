import os
import random
import time

for i in range(10):
    pid = os.fork()
    if pid == 0:
        time.sleep(random.randint(1, 3))
        print(os.getpid(), os.getcwd())
        exit(99)

print("zombie parent exiting")
