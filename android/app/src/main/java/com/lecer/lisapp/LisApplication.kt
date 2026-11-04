package com.lecer.lisapp

import android.app.Application

class LisApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        Lis.init(this)
    }
}
